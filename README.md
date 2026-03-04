# Slipstream

In-memory file editing daemon for LLM agent workflows. Reduces serial file I/O round trips from 10+ to 2-3 via batch operations over a Unix socket.

## The Problem

LLM coding agents (Claude Code, multi-agent orchestrators) spend most of their latency budget on serial file I/O — each read and edit is a full LLM inference round trip (~1-3s). A 5-file refactor can burn 10+ turns just on file operations.

Slipstream provides a persistent background daemon that preloads files into memory, lets agents batch reads and writes in a single tool call, and atomically flushes all changes on session close.

**Status: Completed experiment.** Benchmarks showed the approach works correctly but doesn't provide meaningful speedups at typical editing scales. See [Benchmark Results](#benchmark-results) for details.

## Architecture

```
  +-----------+   +-----------+   +-----------+
  |  Agent A  |   |  Agent B  |   |  Agent C  |
  +-----+-----+   +-----+-----+   +-----+-----+
        |               |               |
        +-------+-------+-------+-------+
                |               |
          Unix Socket (JSON-RPC)
                |               |
  +-------------+---------------+--------------+
  |           slipstream daemon                |
  |                                            |
  |   +------------------------------------+   |
  |   |         Session Manager            |   |
  |   +----------------+-------------------+   |
  |   +----------------v-------------------+   |
  |   |           Buffer Pool              |   |
  |   +----------------+-------------------+   |
  |   +----------------v-------------------+   |
  |   |          Flush Engine              |   |
  |   +------------------------------------+   |
  |                                            |
  +--------------------------------------------+
```

### Crates

| Crate | Binary | Purpose |
|-------|--------|---------|
| `slipstream-core` | — | Buffers, edits, sessions, flush engine |
| `slipstream-daemon` | — | JSON-RPC server over Unix socket |
| `slipstream-cli` | `slipstream` | CLI client for shell/subagent usage |
| `slipstream-mcp` | `slipstream-mcp` | MCP server adapter for Claude Code |

### Key Design Decisions

- **Rust** — performance, safety, single binary
- **Unix domain socket** with newline-delimited JSON-RPC
- **Line-indexed `Vec<String>`** buffers, UTF-8 only, 1MB file size limit
- **Shared buffers, private edits** — multiple sessions share one buffer via `Arc<RwLock>`, edits queue privately per session
- **Optimistic concurrency** with region-aware conflict detection
- **Atomic flush** — temp file + rename, hash-verified
- **`str_replace`** — exact multi-line string matching (added after benchmarks showed line-number indexing is error-prone for LLMs)

## Protocol

### Session Lifecycle

```jsonc
// Open files into memory
→ {"method": "session.open", "params": {"files": ["src/main.rs", "src/lib.rs"]}}
← {"result": {"session_id": "s_01a3", "files": {"src/main.rs": {"lines": 142}}}}

// Flush pending edits to disk
→ {"method": "session.flush", "params": {"session_id": "s_01a3"}}

// Close session (releases resources, aborts unflushed edits)
→ {"method": "session.close", "params": {"session_id": "s_01a3"}}
```

### Batch Operations

The core value proposition — combine multiple reads and writes into a single tool call:

```jsonc
→ {"method": "batch", "params": {
    "session_id": "s_01a3",
    "ops": [
      {"method": "file.read", "path": "src/main.rs", "start": 10, "end": 20},
      {"method": "file.read", "path": "src/lib.rs", "start": 50, "end": 60},
      {"method": "file.str_replace", "path": "src/main.rs",
       "old": "fn old_name()", "new": "fn new_name()"},
      {"method": "file.str_replace", "path": "src/lib.rs",
       "old": "use old_name;", "new": "use new_name;"}
    ]
  }}
```

### Exec (Single-Command Workflow)

For subagents that only have Bash access:

```bash
slipstream exec \
  --files src/main.rs src/lib.rs \
  --read-all \
  --ops '[{"method":"file.str_replace","path":"src/main.rs","old":"foo","new":"bar"}]' \
  --flush
```

## Benchmark Results

Task: apply 8 realistic edits (security fixes, feature wiring, config changes) across 5 Python source files. Averaged over 5 runs.

| | Tool Calls | Wall Time (ms) | Correctness |
|---|---|---|---|
| Traditional (read + edit + write per file) | 18 | 0.31 | 8/8 |
| Slipstream (single exec call) | **1** | 3.68 | 8/8 |

**Tool call reduction: 18 -> 1** (94% fewer round trips).

### What This Means

Each tool call in a real LLM agent workflow costs ~1-3s of inference latency (the model has to process the response and decide the next action). Slipstream eliminates 17 of those round trips, which translates to **~17-51s saved per editing task** in practice.

The raw wall time is higher (3.68ms vs 0.31ms) because of daemon overhead. In a real agent loop though, wall time is dominated by LLM inference, not filesystem I/O — so the metric that matters is how many times the LLM has to stop and think, not how fast the filesystem responds.

### Why It's Still an Experiment

Despite the tool call reduction, Slipstream isn't practical for most use cases because:

1. **Claude Code's built-in tools are good enough.** The Edit tool does str_replace natively. The overhead of running a daemon doesn't pay off unless you're touching 10+ files.
2. **MCP adds latency.** The MCP protocol handshake and JSON serialization add overhead that eats into the round-trip savings at small scale.
3. **Subagent overhead is real.** When accessed via `slipstream exec` from Bash (for agents without MCP access), the process spawn cost dominates.

### Where It Would Win

- Large-scale refactors touching 10+ files simultaneously
- Multi-agent workflows where conflict detection between concurrent editors matters
- Rate-limited API scenarios where minimizing tool calls is critical

Run the benchmark yourself: `python3 docs/benchmark.py`

## Quick Start

```bash
# Start the MCP server
./target/release/slipstream-mcp
```

For Claude Code:

```bash
claude mcp add slipstream -- ./target/release/slipstream-mcp
```

For other MCP clients:

```json
{
  "mcpServers": {
    "slipstream": {
      "command": "/path/to/slipstream-mcp"
    }
  }
}
```

Then use the tools: `slipstream_session("open src/main.rs src/lib.rs")` → `slipstream(ops=[...])` → `slipstream_session("flush")`.

## Building

```bash
cargo build --release
cargo test  # 118 tests across all crates
```

## License

MIT
