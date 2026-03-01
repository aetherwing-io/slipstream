# Slipstream

In-memory file editing daemon for LLM agent workflows. Reduces serial file I/O round trips from 10+ to 2-3 via batch operations over a Unix socket.

**Status: Completed experiment.** Benchmarks showed the approach works correctly but doesn't provide meaningful speedups at typical editing scales. See [Benchmark Results](#benchmark-results) for details.

## The Problem

LLM coding agents (Claude Code, multi-agent orchestrators) spend most of their latency budget on serial file I/O — each read and edit is a full LLM inference round trip (~1-3s). A 5-file refactor can burn 10+ turns just on file operations.

Slipstream provides a persistent background daemon that preloads files into memory, lets agents batch reads and writes in a single tool call, and atomically flushes all changes on session close.

## Architecture

```
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│  Agent A     │  │  Agent B     │  │  Agent C     │
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │                 │                 │
       └────────┬────────┴────────┬────────┘
                │  Unix Socket    │
                │  (JSON-RPC)     │
         ┌──────▼─────────────────▼──────┐
         │        slipstream daemon       │
         │                                │
         │  ┌──────────────────────────┐  │
         │  │      Session Manager     │  │
         │  └───────────┬──────────────┘  │
         │  ┌───────────▼──────────────┐  │
         │  │      Buffer Pool         │  │
         │  └───────────┬──────────────┘  │
         │  ┌───────────▼──────────────┐  │
         │  │      Flush Engine        │  │
         │  └──────────────────────────┘  │
         └────────────────────────────────┘
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

We ran three benchmark iterations comparing Slipstream against Claude Code's traditional Read/Edit tools on identical multi-file editing tasks.

### v1: Line-Indexed Writes

| | Tool Calls | Correctness |
|---|---|---|
| Traditional (Read/Edit) | 13 | 9/9 |
| Slipstream (file.write) | 5 | **6/9** |

Line-number indexing caused LLMs to make off-by-one errors. This led to adding `file.str_replace`.

### v2: String-Match Writes

| | Tool Calls | Time | Tokens | Correctness |
|---|---|---|---|---|
| Traditional (Read/Edit) | 12 | ~25s | ~30K | 7/7 |
| Slipstream (str_replace) | 5 | ~36s | ~30K | **7/7** |

Correctness fixed, but **slower** — the overhead of the daemon and batch protocol exceeds the round-trip savings at this scale.

### v3: Subagent via Bash

| | Tool Calls | Time | Tokens | Correctness |
|---|---|---|---|---|
| Traditional (subagent) | 13 | ~31s | ~29K | 8/8 |
| Slipstream exec (subagent) | 10 | ~156s | ~37K | 7/7 |

The `exec` command works for Bash-only subagents but is significantly slower at small scale.

### Conclusion

Slipstream achieves **fewer tool calls** and **correct results** with `str_replace`, but the latency overhead means it only breaks even when editing many files simultaneously. At typical scales (2-5 files), Claude Code's built-in Read/Edit tools are faster.

The approach would become advantageous for:
- Large-scale refactors touching 10+ files
- Multi-agent workflows where conflict detection matters
- Scenarios where tool-call count is the bottleneck (rate-limited APIs)

## Building

```bash
cargo build --release
```

## Testing

```bash
cargo test
```

118 tests across all crates.

## License

MIT
