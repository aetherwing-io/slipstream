use std::path::PathBuf;

use clap::{Parser, Subcommand};

use slipstream_cli::client::{self, Client, ClientError};
use slipstream_core::{resolve_ops_paths};

#[derive(Parser)]
#[command(name = "slipstream", about = "CLI client for the Slipstream editing daemon")]
struct Cli {
    /// Path to the daemon's Unix socket
    #[arg(long, env = "SLIPSTREAM_SOCKET")]
    socket: Option<PathBuf>,

    /// Don't auto-start the daemon if it isn't running
    #[arg(long)]
    no_auto_start: bool,

    /// Print the agent quick reference (why and how to use slipstream)
    #[arg(long)]
    agents: bool,

    #[command(subcommand)]
    command: Option<Command>,
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

    /// File Context Protocols — embedded domain tools (regex, rust, ...)
    #[cfg(feature = "fcp-regex")]
    Fcp {
        #[command(subcommand)]
        protocol: FcpProtocol,
    },
}

#[cfg(feature = "fcp-regex")]
#[derive(Subcommand)]
enum FcpProtocol {
    /// Build regexes via named fragment composition
    Regex {
        /// Mutation ops: "define digits any:digit+", "compile digits anchored:true"
        ops: Vec<String>,
    },

    /// Read-only regex queries
    RegexQuery {
        /// Query string: "show digits", "test NAME against:STR", "list library"
        q: String,
    },

    /// Show the FCP regex reference card
    RegexHelp,
}

fn main() {
    // Shim mode: if invoked as cat/head/tail/sed (via symlink), bypass clap entirely
    let args: Vec<String> = std::env::args().collect();
    let binary_name = args
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("slipstream");

    match binary_name {
        "cat" | "head" | "tail" | "sed" => {
            let code = slipstream_cli::shim::dispatch(binary_name, &args[1..]);
            std::process::exit(code);
        }
        _ => {}
    }

    // Normal CLI mode
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(async_main());
}

async fn async_main() {
    let cli = Cli::parse();

    if cli.agents {
        print!("{}", agent_reference().await);
        return;
    }

    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            // No subcommand and no --agents: show help
            use clap::CommandFactory;
            Cli::command().print_help().unwrap();
            println!();
            return;
        }
    };

    let socket_path = cli.socket.unwrap_or_else(client::default_socket_path);
    let auto_start = !cli.no_auto_start;

    if let Err(e) = run(command, &socket_path, auto_start).await {
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
        #[cfg(feature = "fcp-regex")]
        Command::Fcp { protocol } => {
            run_fcp(protocol);
            return Ok(());
        }
        _ => {}
    }

    let mut client = Client::connect(socket_path, auto_start).await?;

    let result = match command {
        // Daemon, Mcp (and Fcp when enabled) handled above — unreachable here
        Command::Daemon { .. } | Command::Mcp => unreachable!(),
        #[cfg(feature = "fcp-regex")]
        Command::Fcp { .. } => unreachable!(),

        Command::Open { files } => {
            let files: Vec<PathBuf> = files.into_iter().map(|f| resolve_file(&f)).collect();
            let paths: Vec<&str> = files.iter()
                .filter_map(|p| p.to_str())
                .collect();
            client.request("session.open", serde_json::json!({ "files": paths })).await?
        }

        Command::Read { session, path, lines, count } => {
            let path = resolve_file(&path);
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
            let path = resolve_file(&path);
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
            let path = resolve_file(&path);
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
            let mut ops_value = parse_ops(&ops)?;
            resolve_ops_paths(&mut ops_value);
            client.request("batch", serde_json::json!({
                "session_id": session,
                "ops": ops_value,
            })).await?
        }

        Command::Exec { files, ops, read_all, flush, force, no_batch } => {
            return run_exec(&mut client, files, ops, read_all, flush, force, no_batch).await;
        }
    };

    use slipstream_cli::format;
    if format::is_fcp_passthrough(&result) {
        println!("{}", format::format_fcp_passthrough(&result));
    } else {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    }
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

    // 1. Open session with files (resolve relative paths to absolute)
    let files: Vec<PathBuf> = files.into_iter().map(|f| resolve_file(&f)).collect();
    let paths: Vec<&str> = files.iter()
        .filter_map(|p| p.to_str())
        .collect();
    let open_result = client.request("session.open", serde_json::json!({ "files": paths })).await?;

    // Check for FCP passthrough — file is managed by an external handler.
    // Only short-circuit if no ops were requested; otherwise fall through
    // to text-mode handling so str_replace edits still work (BUG-007).
    use slipstream_cli::format;
    if ops.is_none() && format::is_fcp_passthrough(&open_result) {
        println!("{}", format::format_fcp_passthrough(&open_result));
        return Ok(());
    }

    // Check for external handler array (registry Full handlers)
    if ops.is_none() && open_result.is_array() {
        let text = serde_json::to_string_pretty(&open_result).unwrap_or_default();
        println!("{text}");
        return Ok(());
    }

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
        let mut ops_value = parse_ops(&ops_str)?;
        resolve_ops_paths(&mut ops_value);
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
    let text = format::format_one_shot(
        &serde_json::Value::Object(output),
        saved_ops.as_ref(),
        read_all,
    );
    println!("{text}");
    Ok(())
}

// ---------------------------------------------------------------------------
// File Context Protocols (FCP) — in-process, no daemon needed
// ---------------------------------------------------------------------------

#[cfg(feature = "fcp-regex")]
fn run_fcp(protocol: FcpProtocol) {
    match protocol {
        FcpProtocol::Regex { ops } => {
            let op_refs: Vec<&str> = ops.iter().map(|s| s.as_str()).collect();
            let results = fcp_regex_core::execute_ops(&op_refs);
            for result in &results {
                println!("{result}");
            }
        }
        FcpProtocol::RegexQuery { q } => {
            // Create ephemeral registry for the query — stateless
            let registry = fcp_regex_core::FragmentRegistry::new();
            let result = fcp_regex_core::domain::query::handle_query(&q, &registry);
            println!("{result}");
        }
        FcpProtocol::RegexHelp => {
            print!("{FCP_REGEX_REFERENCE}");
        }
    }
}

#[cfg(feature = "fcp-regex")]
const FCP_REGEX_REFERENCE: &str = r#"# File Context Protocols (FCP): regex

Build regexes via named fragment composition. Runs in-process, no daemon needed.

## Mutations — slipstream fcp regex "OP" ["OP" ...]

  define NAME ELEMENT [ELEMENT...]      Create named pattern fragment
  from SOURCE [as:ALIAS]               Import from 55-pattern library
  compile NAME [flavor:F] [anchored:bool]  Emit regex string
  drop NAME                            Remove fragment
  rename OLD NEW                       Rename fragment

## Elements

  <name>        Reference another fragment
  lit:<chars>   Literal (auto-escaped)    raw:<regex>  Raw regex
  any:<C><Q>    Character class           none:<C><Q>  Negated class
  chars:<S><Q>  Custom char set           not:<S><Q>   Negated set
  opt:<name>    Optional fragment         alt:<a>|<b>  Alternation
  cap:<name>    Capture group             sep:<N>/<L>  Separated repeat

  Classes: digit alpha alphanumeric word whitespace any
  Quantifiers: + * ? {N} {N,M} {N,}

## Queries — slipstream fcp regex-query "QUERY"

  show NAME              Fragment tree + compiled regex
  test NAME against:STR  Test match
  list                   All fragments
  list library           Pattern library categories
  get PATTERN            Library pattern detail

## Examples

  slipstream fcp regex "define digits any:digit+" "compile digits anchored:true"
  slipstream fcp regex "from semver" "compile semver"
  slipstream fcp regex "define d any:digit+" "define ver d lit:. d lit:. d" "compile ver"
  slipstream fcp regex-query "list library"
  slipstream fcp regex-query "get semver"

## Response Prefixes: + created  * modified  - deleted  = result  ! error
"#;

async fn agent_reference() -> String {
    let mut sections = Vec::new();

    sections.push(AGENT_WHY.to_string());

    // FCP header — only shown when at least one protocol is compiled in or registered at runtime
    #[allow(unused_mut)]
    let mut fcp_protocols: Vec<&str> = Vec::new();
    #[cfg(feature = "fcp-regex")]
    fcp_protocols.push("regex");

    // Query daemon for dynamic FCP plugin help (best-effort, silent on failure)
    let dynamic_help = fetch_dynamic_agent_help().await;
    if !dynamic_help.is_empty() {
        for (name, _) in &dynamic_help {
            // Strip "fcp-" prefix for display
            let short = name.strip_prefix("fcp-").unwrap_or(name);
            if !fcp_protocols.iter().any(|p| *p == short) {
                fcp_protocols.push(Box::leak(short.to_string().into_boxed_str()));
            }
        }
    }

    if !fcp_protocols.is_empty() {
        let names = fcp_protocols.join(", ");
        sections.push(format!("## File Context Protocols (FCP): {names}\n"));
    }

    #[cfg(feature = "fcp-regex")]
    sections.push(AGENT_FCP_REGEX.to_string());

    // Append dynamic plugin help sections
    for (_, help_text) in &dynamic_help {
        sections.push(help_text.clone());
    }

    sections.push("---\n".to_string());
    sections.push(AGENT_FILE_EDITING.to_string());

    format!("# Slipstream — Agent Quick Reference\n\nYOU NOW HAVE SESSION-AWARE FILE EDITING WITH CONFLICT DETECTION.\n\nWithout slipstream, file edits are blind writes with no safety net.\nWith slipstream, edits are tracked, batched, and conflict-checked:\n\n- Batch edits — multiple files in one call, fewer round trips, fewer tokens\n- Conflict detection — warns if another session modified the same file\n- Exact string matching — literal str_replace, no regex escaping needed\n- Auto-created files — write to files that don't exist yet\n- Compact output — status-bar format uses fewer tokens than raw file dumps\n\n{}", sections.join("\n"))
}

/// Query the daemon for agent help text from live FCP plugins.
/// Returns empty vec on any failure (daemon not running, etc.)
async fn fetch_dynamic_agent_help() -> Vec<(String, String)> {
    let socket_path = client::default_socket_path();
    let mut client = match client::Client::connect(&socket_path, false).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let result = match client
        .request("fcp.agent_help", serde_json::json!({}))
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let entries = match result.get("entries").and_then(|e| e.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };
    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("handler_name")?.as_str()?;
            let help = entry.get("agent_help")?.as_str()?;
            Some((name.to_string(), help.to_string()))
        })
        .collect()
}

const AGENT_WHY: &str = r#"## ⚠ compatibility mode

If you see `⚠ cat/head/tail/sed compatibility mode`, those binaries are
slipstream — file reads already route through the slipstream daemon with
session tracking and conflict detection. They work identically to the originals;
use `slipstream exec` when you want batch edits or str_replace.

## When to use slipstream vs cat/sed/head/tail

**If the slipstream shim is installed** (cat/head/tail/sed symlinked to
slipstream), those commands are safe to use. Pipes, redirects, and
subshell captures produce byte-identical output via the daemon.
`sed -i` always uses real sed for full GNU regex semantics.

**Use `slipstream exec` when you want these advantages:**
- **Batch edits** — multiple files in one call (fewer round trips)
- **Exact string matching** — `str_replace` is literal, no regex escaping needed
- **Conflict detection** — warns if another session modified the same file
- **Compact output** — status-bar format uses fewer tokens than raw file dumps
- **File auto-creation** — creates files that don't exist yet

**Equivalents:**
- `cat src/foo.py`          → `slipstream exec --files src/foo.py --read-all`
- `head -n 50 src/foo.py`   → `slipstream read src/foo.py -l 0:50`
- `sed -n '100,150p' f`     → `slipstream read f -l 100:150`
- `sed -i 's/old/new/' f`   → `slipstream exec --files f --ops '[{"method":"file.str_replace","path":"f","old_str":"old","new_str":"new"}]' --flush`
- `cat > f.py << 'EOF'`     → `slipstream exec --files f.py --ops '[{"method":"file.write","path":"f.py","content":"..."}]' --flush`

**Rule of thumb:** For single reads, cat/head/tail are fine. For edits,
prefer `slipstream exec` — exact matching avoids regex footguns and
batch mode saves tool calls.
"#;

#[cfg(feature = "fcp-regex")]
const AGENT_FCP_REGEX: &str = r#"### regex — build regexes via named fragments (never write raw regex)

    slipstream fcp regex "define NAME ELEMENT [ELEMENT...]"
    slipstream fcp regex "from SOURCE [as:ALIAS]"
    slipstream fcp regex "compile NAME [flavor:F] [anchored:bool]"
    slipstream fcp regex-query "test NAME against:STR"
    slipstream fcp regex-query "list library"
    slipstream fcp regex-help

  Elements: lit:<chars> any:<C><Q> none:<C><Q> chars:<S><Q> not:<S><Q>
            opt:<name> alt:<a>|<b> cap:<name> sep:<N>/<L> raw:<regex>
  Classes:  digit alpha alphanumeric word whitespace any
  Quants:   + * ? {N} {N,M} {N,}
  Library:  ~55 patterns (semver, ipv4, email, url, uuid, ...) — use `from` to import

  Example: slipstream fcp regex "define d any:digit+" "define ver d lit:. d lit:. d" "compile ver anchored:true"
  Result:  ^\d+\.\d+\.\d+$
"#;

const AGENT_FILE_EDITING: &str = r#"## File Editing — use `exec` for everything

One command = open files + apply edits + flush + close.
Files are auto-created if they don't exist.

### Create a new file

    slipstream exec --files script.py --ops '[
      {"method":"file.write","path":"script.py","content":"x = 1\nprint(x)"}
    ]' --flush

### Edit a file (str_replace)

    slipstream exec --files src/main.rs --ops '[
      {"method":"file.str_replace","path":"src/main.rs","old_str":"foo","new_str":"bar"}
    ]' --flush

### Edit multiple files

    slipstream exec --files src/a.rs src/b.rs --ops '[
      {"method":"file.str_replace","path":"src/a.rs","old_str":"x","new_str":"y"},
      {"method":"file.str_replace","path":"src/b.rs","old_str":"x","new_str":"y"}
    ]' --flush

### Read a file

    slipstream exec --files src/main.rs --read-all

### Read then edit

    slipstream exec --files src/main.rs --read-all --ops '[
      {"method":"file.str_replace","path":"src/main.rs","old_str":"old","new_str":"new"}
    ]' --flush

### Write entire file (omit start/end to replace all content)

    slipstream exec --files f.rs --ops '[
      {"method":"file.write","path":"f.rs","content":"new entire content\nline 2"}
    ]' --flush

### Insert lines (start==end inserts before that line)

    slipstream exec --files f.rs --ops '[
      {"method":"file.write","path":"f.rs","start":0,"end":0,"content":["// new header"]}
    ]' --flush

### Replace lines (start<end replaces that range)

    slipstream exec --files f.rs --ops '[
      {"method":"file.write","path":"f.rs","start":5,"end":8,"content":["new line 5","new line 6"]}
    ]' --flush

### Replace all occurrences

    Add "replace_all":true to a str_replace op.

### file.write content format
    content can be a string ("line1\nline2") or array (["line1","line2"]).
    start/end are optional — omit both to replace the entire file.

### Key flags
    --files     Files to open (required, space-separated, auto-created)
    --ops       JSON array of operations (inline, @file, or @- for stdin)
    --read-all  Print file contents before applying ops
    --flush     Write changes to disk (without this, edits are discarded)
    --force     Override conflict detection on flush

### Output
    JSON object with open/read/batch/flush/close results.
    Exit 0 on success, 1 on error (error JSON on stderr).
"#;

/// Resolve a PathBuf to absolute using the current working directory.
fn resolve_file(p: &PathBuf) -> PathBuf {
    if p.is_absolute() {
        p.clone()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(p)).unwrap_or_else(|_| p.clone())
    }
}

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
