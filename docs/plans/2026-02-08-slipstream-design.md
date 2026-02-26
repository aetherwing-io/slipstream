# Slipstream: In-Memory File Editing Daemon for Agent Workflows

## Context

LLM coding agents (Claude Code, multi-agent orchestrators) spend most of their latency budget on serial file I/O — each read and edit is a full LLM inference round trip (~1-3s). A 5-file refactor can burn 10+ turns just on file operations. Additionally, multi-agent setups have no coordination mechanism for concurrent file access, leading to silent edit conflicts.

Slipstream solves this by providing a persistent background daemon that preloads files into memory, lets agents work on slices via a protocol, and atomically flushes all changes on session close — reducing 10+ round trips to 2-3.

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

- **Language:** Rust
- **Transport:** Unix domain socket, JSON-RPC protocol
- **Session Manager:** Tracks active sessions, cursors, pending edit stacks. Sessions auto-expire after configurable inactivity timeout (default: 5 min).
- **Buffer Pool:** Shared in-memory file contents, reference-counted, loaded on first access. Files above configurable size limit (default: 1MB) are rejected with a clear error directing agents to use direct filesystem access.
- **Flush Engine:** Region-aware conflict detection, atomic disk writes (temp + rename)

## Data Model

```rust
BufferPool {
    buffers: HashMap<PathBuf, Arc<RwLock<FileBuffer>>>,
    max_file_size: usize,     // configurable, default 1MB
}

FileBuffer {
    path: PathBuf,
    lines: Vec<String>,       // line-indexed (text files only, UTF-8)
    version: u64,             // incremented on every flush
    disk_hash: u64,           // detect external modifications
    ref_count: usize,
}

Session {
    id: SessionId,
    files: HashMap<PathBuf, FileHandle>,
    status: Open | Flushing | Closed,
    created_at: Instant,
    last_activity: Instant,   // for inactivity timeout
}

FileHandle {
    buffer: Arc<RwLock<FileBuffer>>,
    snapshot_version: u64,    // version when session opened this file
    cursor: Cursor,
    edits: Vec<Edit>,         // pending modifications
}

Cursor { line: usize }

Edit {
    range: (usize, usize),   // (start_line, end_line)
    content: Vec<String>,     // replacement lines
    timestamp: Instant,
}
```

Key decisions:
- **Shared buffers, private edits** — multiple sessions share one buffer via `Arc<RwLock>`, but edits queue privately per session
- **Optimistic concurrency** — sessions record buffer version on open, check on flush
- **Line-indexed** — agents think in lines, not bytes. Binary files are out of scope.
- **UTF-8 only** — encoding detection deferred to v2
- **File size limit** — reject files above threshold to prevent OOM; large files use direct filesystem access

## Protocol

### Session Lifecycle

```jsonc
// Open — preload files into memory
→ {"method": "session.open", "params": {"files": ["src/main.rs", "src/lib.rs"]}}
← {"result": {"session_id": "s_01a3", "files": {
     "src/main.rs": {"lines": 142, "version": 1},
     "src/lib.rs": {"lines": 89, "version": 3}
   }}}
// Error if file exceeds size limit:
← {"error": {"code": 413, "message": "file too large", "data": {
     "path": "huge.json", "size_bytes": 5242880, "limit_bytes": 1048576,
     "hint": "Use direct filesystem access for files above the size limit"
   }}}

// Flush — apply all pending edits to disk atomically
→ {"method": "session.flush", "params": {"session_id": "s_01a3"}}
← {"result": {"status": "ok", "files_written": 2}}

// Flush with force — apply even on conflict (agent accepts responsibility)
→ {"method": "session.flush", "params": {"session_id": "s_01a3", "force": true}}

// Close — release resources (aborts any unflushed edits)
→ {"method": "session.close", "params": {"session_id": "s_01a3"}}
```

### Reading

```jsonc
// Read a slice by line range
→ {"method": "file.read", "params": {
     "session_id": "s_01a3", "path": "src/main.rs",
     "start": 40, "end": 60
   }}
← {"result": {"lines": [...], "cursor": 60,
     "other_sessions": [{"session": "s_02b4", "dirty_ranges": [[12, 18]]}]
   }}

// Read from cursor (advance N lines)
→ {"method": "file.read", "params": {
     "session_id": "s_01a3", "path": "src/main.rs", "count": 20
   }}
← {"result": {"lines": [...], "cursor": 80}}

// Move cursor
→ {"method": "cursor.move", "params": {
     "session_id": "s_01a3", "path": "src/main.rs", "to": 0
   }}
```

### Writing

```jsonc
// Replace a line range (queued, not flushed)
→ {"method": "file.write", "params": {
     "session_id": "s_01a3", "path": "src/main.rs",
     "start": 45, "end": 50,
     "content": ["    let config = Config::new();", "    config.init();"]
   }}
← {"result": {"edits_pending": 1}}

// Insert at a line (zero-width range)
→ {"method": "file.write", "params": {
     "session_id": "s_01a3", "path": "src/main.rs",
     "start": 45, "end": 45,
     "content": ["// inserted line"]
   }}
```

### Batch Operations (the round-trip killer)

```jsonc
→ {"method": "batch", "params": {"session_id": "s_01a3", "ops": [
     {"method": "file.read", "path": "src/main.rs", "start": 1, "end": 30},
     {"method": "file.read", "path": "src/lib.rs", "start": 10, "end": 25},
     {"method": "file.write", "path": "src/main.rs", "start": 15, "end": 20,
      "content": ["// replaced"]}
   ]}}
← {"result": [{"lines": [...]}, {"lines": [...]}, {"edits_pending": 1}]}
```

### Concurrent Change Awareness

Every response includes `other_sessions` showing what other sessions have pending edits in the same files. Agents see this naturally and can avoid conflicting regions without any special notification protocol.

## Conflict Resolution (Flush Engine)

On `session.flush`:

1. Lock the file buffer (brief write lock)
2. Compare `snapshot_version` vs current buffer `version`
3. If version mismatch: compare edit ranges for overlap
   - No overlap → safe, apply both sets of changes
   - Overlap + `force: false` → CONFLICT, abort this file, return details
   - Overlap + `force: true` → apply edits anyway (agent accepts responsibility, last-write-wins)
4. Sort edits bottom-up (highest line numbers first to avoid offset cascading)
5. Apply edits to buffer, increment version
6. Write to disk atomically (write temp file → rename)
7. Unlock

On conflict, the response includes exact conflicting ranges:
```jsonc
{"error": {"code": 409, "data": {
  "path": "src/main.rs",
  "your_edits": [[45, 50]],
  "conflicting_edits": [[42, 55]],
  "by_session": "s_02b4",
  "hint": "Re-read conflicting ranges and retry, or use force:true to overwrite"
}}}
```

## Project Structure

```
slipstream/
├── Cargo.toml
├── crates/
│   ├── slipstream-core/        # Buffer pool, sessions, edits, flush engine
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── buffer.rs       # FileBuffer, BufferPool
│   │       ├── session.rs      # Session, FileHandle, Cursor
│   │       ├── edit.rs         # Edit, conflict detection
│   │       └── flush.rs        # Flush engine, atomic disk writes
│   ├── slipstream-daemon/      # Unix socket server, JSON-RPC handler
│   │   └── src/
│   │       ├── main.rs         # Daemon entry, socket listener
│   │       ├── protocol.rs     # JSON-RPC parsing, dispatch
│   │       └── handler.rs      # Method handlers → core calls
│   └── slipstream-cli/         # Client CLI for agent integration + testing
│       └── src/
│           └── main.rs
├── tests/
│   ├── integration/            # Multi-session conflict tests
│   └── bench/                  # Latency benchmarks
└── docs/
    └── protocol.md
```

## Implementation Phases

### Phase 1 — Core Library (`slipstream-core`)
- `BufferPool`: load files, ref-count, shared access, file size limit enforcement
- `Session`: create/destroy, file handle management, inactivity tracking
- `FileHandle`: cursor movement, edit queuing
- Pure library, no I/O beyond initial file reads. Fully unit-testable.

### Phase 2 — Flush Engine
- Edit resolution: sort bottom-up, apply to buffer
- Conflict detection: version check + range overlap analysis
- `force` flag: last-write-wins when agent opts in
- Atomic disk writes: temp file + rename
- Integration tests: two sessions editing same file, overlapping vs non-overlapping ranges

### Phase 3 — Daemon
- Unix socket listener (tokio)
- JSON-RPC protocol parser
- Method dispatch to core library
- Session lifecycle: inactivity timeout (configurable, default 5 min), automatic cleanup

### Phase 4 — Batch & Change Awareness
- `batch` method: execute multiple ops in one call
- `other_sessions` field in all responses
- Integration tests: batch correctness, concurrent session visibility

### Phase 5 — CLI Client (primary agent integration)
- Commands: `open`, `read`, `write`, `flush`, `close`, `status`, `batch`
- Designed for agent invocation via Bash tool (e.g., `slipstream open src/main.rs`)
- JSON output mode for machine consumption
- Auto-starts daemon if not running
- Useful for debugging, demos, and as the first integration path for agents

### Phase 6 — MCP Server Adapter (future)
- Thin MCP server wrapping the Unix socket protocol
- Exposes tools: `slipstream_open`, `slipstream_read`, `slipstream_write`, `slipstream_batch`, `slipstream_flush`, `slipstream_close`
- Seamless Claude Code integration without Bash intermediary

## Verification

- **Unit tests:** Core library — buffer operations, edit application, cursor movement, file size limit rejection
- **Conflict tests:** Two sessions, same file, overlapping ranges → expect 409; non-overlapping → expect success; force flag → expect success with warning
- **Atomic write tests:** Kill daemon mid-flush → file should be either old or new, never partial
- **Session timeout tests:** Open session, wait past timeout, verify cleanup
- **Bench:** Measure latency of session.open → batch read/write → session.flush vs equivalent filesystem ops
- **CLI smoke test:** Open session, read slices, write edits, flush, verify file on disk matches expected output
- **E2E agent test:** Claude Code agent uses CLI to perform a multi-file refactor through slipstream

## Design Review Notes (Gemini Debate)

Accepted from review:
- File size limit (1MB default) to prevent OOM
- `force` flag on flush for conflict escape hatch
- Session inactivity timeout for resource cleanup
- Renamed session.close → session.flush (commit) + session.close (release)

Deferred to v2:
- Crash recovery / write-ahead log
- Non-UTF-8 encoding support
- Multi-user auth/security model

Rejected:
- Binary protocol (JSON-RPC parsing overhead negligible vs LLM inference latency)
- LSP integration (out of scope — slipstream is a file buffer, not a code intelligence engine)
- CRDT/OT merging (reservation layer + other_sessions awareness + force flag is sufficient)
