# Slipstream: File Session Coordinator

## Summary

Evolve slipstream from "in-memory text file editor" to **file session coordinator**. Slipstream keeps its native text editing (unchanged) but adds a format registry, format-aware open, global session digest, and coordinated lifecycle management across registered file handlers (FCP servers, mish, and others).

Slipstream does NOT proxy format-specific operations. It does NOT embed other servers. It provides **file awareness, guidance, and concurrency coordination** — the layer that tells the LLM what tools to use and tracks the state of everything that's open.

## Context

### The Three Projects

| Project | Role | Language | Status |
|---------|------|----------|--------|
| **slipstream** | In-memory file I/O, sessions, batch ops, str_replace | Rust | Built, 118 tests |
| **FCP** | MCP servers for complex file formats (drawio, midi, sheets, terraform) | TS/Python/Go | Built, 1000+ tests |
| **mish** | LLM-native shell, process supervision, PTY capture | Rust | Design phase |

### The Problem

An LLM working on a project juggles files across multiple tools simultaneously. It has no unified view of what's open, who's handling what, or what state things are in. When context gets compacted, file state is lost entirely. The LLM doesn't know that a `.drawio` file should be opened with fcp-drawio, or that pending text edits should be flushed before running a build.

### Why Not a Universal Proxy?

A debate council examined the option of making slipstream route FCP operations. Evidence against:

- Slipstream's core is `Vec<String>` line-indexed buffers — incommensurable with FCP's domain models (graphs, event streams, ASTs)
- FCP servers hold rich semantic models with event-sourced undo/redo — slipstream has no undo
- Polyglot subprocess management (Node, Python, Go from Rust) creates cascading failure modes
- Cross-format batching (edit a .py AND a .mid in one call) doesn't match how LLMs actually work — tasks are modal

The right integration is not nesting. It's coordination.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     SLIPSTREAM COORDINATOR                       │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Format Registry                                         │   │
│  │  .py → native (text)     .xlsx → fcp-sheets              │   │
│  │  .rs → native (text)     .drawio → fcp-drawio            │   │
│  │  .ts → native (text)     .mid → fcp-midi                 │   │
│  │  .tf → fcp-terraform     Makefile → mish (advisory)      │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Session Tracker (global state)                          │   │
│  │                                                           │   │
│  │  main.py        native   session:abc  3 edits pending    │   │
│  │  utils.py       native   session:abc  clean (flushed)    │   │
│  │  report.xlsx    sheets   external     managed externally │   │
│  │  arch.drawio    drawio   external     managed externally │   │
│  │  infra.tf       terraform external    managed externally │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
│  ┌──────────────────┐  ┌─────────────────────────────────────┐  │
│  │  Native Text     │  │  Coordination Engine                │  │
│  │  Editor          │  │                                     │  │
│  │  (existing code) │  │  - Format-aware open guidance       │  │
│  │  - buffer.rs     │  │  - Global session digest            │  │
│  │  - edit.rs       │  │  - Flush-before-build warnings      │  │
│  │  - session.rs    │  │  - Conflict advisory                │  │
│  │  - str_match.rs  │  │  - Context recovery (status cmd)    │  │
│  │  - flush.rs      │  │                                     │  │
│  └──────────────────┘  └─────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Components

### 1. Format Registry

A configuration file mapping file extensions to handlers. Loaded at daemon startup.

**Location:** `~/.config/slipstream/formats.toml` (user-configurable) with a built-in default set compiled into the binary.

**Schema:**

```toml
[handlers.sheets]
extensions = ["xlsx", "xls"]
tool_prefix = "sheets"
session_open = 'sheets_session("open {path}")'
session_save = 'sheets_session("save")'
help_tool = "sheets_help()"
description = "Spreadsheet editing — set cells, style ranges, add charts"
# Optional: example operations for guidance
examples = [
  'sheets(["set A1 Revenue"])',
  'sheets(["style A1:D1 bold fill:#4472C4"])',
]

[handlers.drawio]
extensions = ["drawio", "dio"]
tool_prefix = "drawio"
session_open = 'drawio_session("open {path}")'
session_save = 'drawio_session("save")'
help_tool = "drawio_help()"
description = "Diagram editing — add shapes, connect nodes, layout"
examples = [
  'drawio(["add svc AuthService theme:blue"])',
  'drawio(["connect Auth -> DB"])',
]

[handlers.midi]
extensions = ["mid", "midi"]
tool_prefix = "midi"
session_open = 'midi_session("open {path}")'
session_save = 'midi_session("save")'
help_tool = "midi_help()"
description = "MIDI composition — notes, chords, tracks, instruments"
examples = [
  'midi(["note Piano C4 at:1.1 dur:quarter"])',
  'midi(["chord Piano Cmaj at:1.1 dur:half"])',
]

[handlers.terraform]
extensions = ["tf", "tfvars"]
tool_prefix = "terraform"
session_open = 'terraform_session("open {path}")'
session_save = 'terraform_session("save")'
help_tool = "terraform_help()"
description = "Terraform HCL generation — providers, resources, variables"
examples = [
  'terraform(["add provider aws region:us-east-1"])',
  'terraform(["add resource aws_instance web"])',
]

# Advisory-only handlers (no session management, just guidance)
[handlers.make]
extensions = ["Makefile", "makefile", "GNUmakefile"]
advisory = true
description = "Build file — run with mish or sh_run"
guidance = 'Execute with: sh_run(cmd="make [target]")'

[handlers.docker]
extensions = ["Dockerfile"]
advisory = true
description = "Container definition — build/run with mish or sh_run"
guidance = 'Build: sh_run(cmd="docker build .")'
```

**Key design decisions:**
- TOML for human readability and easy editing
- `advisory = true` for handlers that only provide guidance (no session management)
- `examples` field gives the LLM concrete patterns, not abstract descriptions
- Template variable `{path}` in session_open for path interpolation
- Registry is extensible — users add their own handlers for any format

### 2. Format-Aware Open

When `session.open` is called with a file path, slipstream checks the format registry BEFORE attempting to load the file as text.

**Behavior matrix:**

| File Extension | Registry Match | Action |
|---------------|---------------|--------|
| `.py`, `.rs`, `.ts` | No match (or matches `native`) | Load as text buffer (existing behavior) |
| `.xlsx` | Matches `sheets` handler | Return guidance response, register as externally-managed |
| `.drawio` | Matches `drawio` handler | Return guidance response, register as externally-managed |
| `.mid` | Matches `midi` handler | Return guidance response, register as externally-managed |
| Unknown extension | No match | Attempt to load as text (existing behavior) |

**Guidance response format** (for externally-managed files):

```json
{
  "status": "external_handler",
  "path": "/path/to/report.xlsx",
  "handler": "sheets",
  "description": "Spreadsheet editing — set cells, style ranges, add charts",
  "instructions": {
    "open": "sheets_session(\"open /path/to/report.xlsx\")",
    "save": "sheets_session(\"save\")",
    "help": "sheets_help()",
    "examples": [
      "sheets([\"set A1 Revenue\"])",
      "sheets([\"style A1:D1 bold fill:#4472C4\"])"
    ]
  },
  "tracking_id": "ext-001"
}
```

The file is NOT loaded into a text buffer. It is registered in the session tracker as externally-managed with a tracking ID.

**For advisory handlers** (Makefile, Dockerfile):

```json
{
  "status": "advisory",
  "path": "/path/to/Makefile",
  "handler": "make",
  "guidance": "Execute with: sh_run(cmd=\"make [target]\")",
  "loaded_as_text": true
}
```

Advisory files ARE loaded as text (they're readable text files) but also include guidance about how to execute/use them.

### 3. Session Tracker

A global data structure that tracks ALL files the coordinator knows about, regardless of handler.

**Data model:**

```rust
pub struct TrackedFile {
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    pub handler: HandlerType,
    pub state: FileState,
    pub tracking_id: String,
    pub registered_at: Instant,
    pub last_activity: Instant,
}

pub enum HandlerType {
    /// Managed natively by slipstream's text buffer system
    Native { session_id: SessionId },
    /// Managed by an external FCP server or other tool
    External { handler_name: String },
    /// Advisory only — loaded as text, with usage guidance
    Advisory { handler_name: String, session_id: SessionId },
}

pub enum FileState {
    /// Native: file loaded, no pending edits
    Clean,
    /// Native: file has unflushed edits
    Dirty { edit_count: usize },
    /// Native: edits flushed to disk
    Flushed,
    /// External: registered but managed by another tool
    ExternallyManaged,
    /// Closed/released
    Closed,
}
```

**Implementation:** `DashMap<PathBuf, TrackedFile>` (consistent with existing slipstream patterns). The session tracker is separate from the existing `SessionManager` — it's a coordinator-level structure that includes both native sessions and external registrations.

### 4. Global Session Digest

Every response from slipstream includes a digest of all tracked files. This gives the LLM ambient awareness without polling.

**Digest format:**

```
files: 5 tracked | 2 native (1 dirty) | 3 external
  main.py        native   3 edits pending
  utils.py       native   clean
  report.xlsx    sheets   externally-managed
  arch.drawio    drawio   externally-managed
  infra.tf       terraform externally-managed
```

**When to include the digest:**
- After every mutation response (open, write, str_replace, flush, close)
- On explicit `status` query
- NOT on read-only operations (file.read) to keep responses lean

**Digest data:**

```rust
pub struct SessionDigest {
    pub total_tracked: usize,
    pub native_count: usize,
    pub native_dirty: usize,
    pub external_count: usize,
    pub files: Vec<DigestEntry>,
}

pub struct DigestEntry {
    pub path: String,        // relative to CWD if possible
    pub handler: String,     // "native", "sheets", "drawio", etc.
    pub state: String,       // "3 edits pending", "clean", "externally-managed"
}
```

### 5. Coordination Commands

New JSON-RPC methods for the coordination layer:

#### `coordinator.status`

Returns the full session digest. The LLM's "where am I?" recovery command.

```json
// Request
{"method": "coordinator.status"}

// Response
{
  "tracked_files": [...],
  "native_sessions": [...],
  "external_registrations": [...],
  "warnings": ["main.py has 3 unflushed edits"]
}
```

#### `coordinator.register`

Allows external tools to register a file they're managing. Called by the LLM after opening a file in an FCP server.

```json
// Request
{"method": "coordinator.register", "params": {
  "path": "/path/to/report.xlsx",
  "handler": "sheets"
}}

// Response
{"tracking_id": "ext-001", "status": "registered"}
```

#### `coordinator.unregister`

Remove a file from tracking (after the FCP server saves and closes it).

```json
// Request
{"method": "coordinator.unregister", "params": {
  "tracking_id": "ext-001"
}}
```

#### `coordinator.check`

Pre-flight check before an action. The LLM asks "is it safe to build?"

```json
// Request
{"method": "coordinator.check", "params": {
  "action": "build"
}}

// Response
{
  "warnings": [
    "main.py has 3 unflushed edits — flush session abc123 first",
    "infra.tf is externally managed by terraform — ensure it's saved"
  ],
  "suggestion": "Run: slipstream_flush(session_id: \"abc123\") then terraform_session(\"save\")"
}
```

### 6. MCP Tool Updates

The existing slipstream MCP server gains new tools:

| Tool | Purpose |
|------|---------|
| `slipstream_status` | Return full coordinator status (digest + warnings) |
| `slipstream_register` | Register an externally-managed file |
| `slipstream_unregister` | Remove external file from tracking |
| `slipstream_check` | Pre-flight check before an action |

Existing tools (`slipstream_open`, `slipstream_exec`, etc.) are modified to:
1. Check the format registry before loading files
2. Return guidance for registered formats
3. Append the session digest to mutation responses

## Concurrency Model

### Advisory Conflict Detection

When a file is tracked (either native or external), slipstream warns about potential conflicts:

- If the LLM tries to `file.write` to a path that's registered as externally-managed: **warn** that the file is managed by another tool
- If the LLM opens a file natively that's already registered externally: **warn** about the conflict
- If disk content changes under a native session (detected on flush via hash comparison): **warn** about external modification (existing behavior)

All warnings are advisory, not blocking. The LLM decides what to do.

### Coordinated Flush Advisory

When the LLM calls `coordinator.check(action: "build")`, slipstream examines all tracked files and returns a checklist:
- Native files with pending edits → "flush these"
- External files → "ensure these are saved (call their save command)"
- No pending changes → "ready to build"

This is guidance, not enforcement. Slipstream tells the LLM what to do; the LLM executes it.

## What Does NOT Change

- **slipstream-core**: `FileBuffer`, `Edit`, `Session`, `flush` — all unchanged. Still `Vec<String>`, still line-indexed, still text-only.
- **Existing behavior**: All current slipstream operations work exactly as before for text files.
- **FCP servers**: No changes required. They don't know about slipstream. The registry is slipstream-side configuration.
- **Protocol**: JSON-RPC over Unix socket remains the transport. New methods are additive.

## Implementation Plan

### Phase 1: Format Registry + Format-Aware Open
- Parse `formats.toml` at daemon startup
- Modify `handle_session_open` to check registry before loading
- Return guidance response for registered formats
- Built-in default registry for FCP formats

### Phase 2: Session Tracker
- `TrackedFile` data model + `DashMap` storage
- Register native files on open (existing sessions feed into tracker)
- Register external files via `coordinator.register`
- Unregister on close/unregister

### Phase 3: Global Session Digest
- `SessionDigest` struct
- Append digest to mutation responses
- `coordinator.status` command

### Phase 4: Coordination Commands
- `coordinator.check` with pre-flight logic
- Warning generation for conflicts
- Flush advisory for builds

### Phase 5: MCP Tool Updates
- New tools: `slipstream_status`, `slipstream_register`, `slipstream_unregister`, `slipstream_check`
- Modify existing tools to include digest
- Update tool descriptions for LLM consumption

### Phase 6: Testing + Integration
- Unit tests for format registry parsing
- Unit tests for session tracker
- Unit tests for digest generation
- Integration tests for format-aware open flow
- Integration tests for coordination commands
- E2E test: open text + register external + check + flush advisory

## Future Extensions (Not in This Design)

- **Active coordination**: Slipstream actually calls FCP servers to flush/save (requires subprocess management — deferred)
- **File watching**: Slipstream detects on-disk changes to tracked files via filesystem events
- **MCP server discovery**: Auto-detect running MCP servers and their capabilities instead of static config
- **mish integration**: mish registers its process state with slipstream for unified status
- **Cross-project coordination**: Multiple slipstream instances share state for multi-repo workflows
