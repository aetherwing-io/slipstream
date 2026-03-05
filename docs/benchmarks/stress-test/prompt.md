# Slipstream Stress Test & Capability Map

## Context

Slipstream is a Rust daemon for in-memory file editing in LLM agent workflows. It's configured as an MCP server (`slipstream` in `.mcp.json`). The project lives at `/Users/scottmeyer/projects/slipstream`.

### Tool Surface (4 tools — FCP pattern)

| Tool | Purpose | Key params |
|------|---------|------------|
| `slipstream` | Main ops (one-shot or session mode) | `files`, `session`, `ops`, `read_all`, `flush`, `force` |
| `slipstream_session` | Lifecycle DSL | `action`: "open ...", "flush", "close", "register ...", "unregister ..." |
| `slipstream_query` | Read-only DSL | `q`: "read ...", "status", "list", "check build" |
| `slipstream_help` | Reference card | (none) |

**Two modes for `slipstream`:**
- **One-shot** (`files` provided): open → read? → ops? → flush? → close. Self-contained.
- **Session** (`files` omitted): ops run on active named session from `slipstream_session('open ...')`.

**Named sessions:** Default is implicit. For concurrent agents, use `as:NAME` on open and `session:NAME` on other actions.

Previous benchmarks showed Slipstream matches traditional Read/Edit correctness (7/7) with fewer tool calls (5 vs 12), but with higher latency at small file counts. We've never stress-tested the edges. That's what this session is for.

## Goal

Systematically exercise every Slipstream capability, push limits, document what works and what breaks. Produce a structured report at the end.

## Test Protocol

For each test: record the tool calls, timing, success/failure, and any error messages. Use a scratch directory (`/tmp/slipstream-stress/`) for test files. Create test fixtures as needed.

Run tests in order. If a test fails, note the failure and continue — don't stop.

---

## Test Suite

### T1: Basic Lifecycle Smoke Test
`slipstream_session('open <file>')` → `slipstream_query('read <file>')` → `slipstream(ops=[str_replace])` → `slipstream_session('flush')` → `slipstream_session('close')`. Confirm the edit persists on disk. This is the baseline — if this fails, stop.

### T2: Multi-File Session (Scale)
Create 30 small files (10 lines each). `slipstream_session('open f1 f2 ... f30')`. `slipstream(ops=[...], read_all=true)` with one str_replace per file. `slipstream_session('flush')`. Verify all 30 edits landed. This tests near the MAX_FILES_PER_SESSION=32 limit.

### T3: File Count Limit (33 files)
Try `slipstream_session('open f1 ... f33')`. Should hit the 32-file limit. Document the error message. Is it clear? Is it an RPC error or a crash?

### T4: Large File (Near 1MB)
Generate a file just under 1MB (~900KB of text). Open via session, `slipstream_query('read <file> start:100 end:110')`, apply str_replace in the middle, flush. Does it work? How's the latency?

### T5: Large File (Over 1MB)
Generate a file over 1MB. Try to open it. Document the error. Is it graceful?

### T6: Batch Throughput
Open 5 files. Send a single `slipstream(ops=[...])` with 50 operations (mix of reads and str_replaces across all 5 files). Flush. Time it. Compare to doing the same operations individually.

### T7: Batch Limit (257 ops)
Send `slipstream(ops=[...])` with 257 operations. Should hit MAX_BATCH_OPS=256. Document the error.

### T8: Content Size Limit
Open a file. Try to write 60,000 lines in a single write op (exceeds MAX_CONTENT_LINES_PER_WRITE=50,000). Document the error.

### T9: Concurrent Named Sessions
Open 3 named sessions: `slipstream_session('open f1 f2 as:agent-a')`, `slipstream_session('open f3 f4 as:agent-b')`, `slipstream_session('open f5 f6 as:agent-c')`. Apply ops to each via `slipstream(session="agent-a", ops=[...])`. Flush all 3. Verify no cross-contamination. This tests the named session map + DashMap sharding.

### T10: Shared File Conflict
Open the same file in 2 named sessions. Edit different regions in each. `slipstream_session('flush session:a')`. Then `slipstream_session('flush session:b')` (should detect conflict). Document the conflict response. Then `slipstream_session('flush --force session:b')`.

### T11: str_replace Ambiguity
Open a file with repeated text blocks. Try str_replace on text that appears 3 times (without replace_all). Should error. Then retry with replace_all=true. Then try with text that appears 0 times.

### T12: One-Shot Mode
Use `slipstream(files=[...], read_all=true, ops=[...], flush=true)` to open 5 files, read all, apply 5 str_replace ops, and flush — all in one tool call. Verify correctness.

### T13: One-Shot Without Flush
Use `slipstream(files=[...], ops=[...], flush=false)`. Verify the file on disk is unchanged (edits not persisted, session auto-closed).

### T14: Coordinator Status
Open a session, make edits (don't flush). `slipstream_query('status')`. Does the digest accurately reflect dirty state? `slipstream_query('check build')`. Does it warn about unflushed edits?

### T15: External File Registration
`slipstream_session('register /tmp/fake.xlsx sheets')`. `slipstream_query('status')` — is it tracked? `slipstream_session('unregister <id>')`. `slipstream_query('status')` again — is it gone?

### T16: Invalid Handler Name
`slipstream_session('register ../../etc/passwd ../../bin/sh')`. Should be rejected by handler name validation.

### T17: Session Timeout
Open a session. Wait (or note that the sweeper runs every 30s with a 5-min timeout). Check if the session is still accessible after a few operations spread over time.

### T18: Empty File
Open a zero-byte file. `slipstream_query('read <file>')`. str_replace on it (should fail — no text to match). Write to it via `slipstream(ops=[file.write])`. Flush. Verify.

### T19: Binary/Non-UTF8 File
Try to open a binary file (e.g., the slipstream-daemon binary itself). Should fail with a NotUtf8 error. Is the error message clean?

### T20: Rapid Open/Close Cycling
Open and close 50 sessions in rapid succession (loop via Bash + CLI exec, or repeated session open/close). Check for resource leaks — does the daemon stay responsive? `slipstream_query('list')` returns empty?

### T21: Cursor Operations
Open a file. `slipstream(ops=[{"method": "cursor.move", "path": "...", "to": 50}])`. `slipstream_query('read <file> count:10')`. Move cursor to 0. Read 5 lines. Verify cursor state is tracked correctly.

### T22: Subagent via Bash (exec CLI)
Spawn a subagent that only has Bash access. Have it use the `slipstream` CLI binary to exec a read+edit+flush workflow. This tests the CLI path that subagents use.

### T23: Parallel Subagent Edits
Spawn 3 subagents, each editing different files via `slipstream exec` (Bash). Run them in parallel. Verify all edits land correctly with no corruption.

### T24: Default Session Implicit Behavior
`slipstream_session('open f.rs')` (no `as:` qualifier). `slipstream(ops=[...])` (no `session` param). Verify it routes to the default session. `slipstream_session('flush')`. `slipstream_session('close')`.

### T25: Session Name Collision
`slipstream_session('open f.rs as:worker')`. Then `slipstream_session('open g.rs as:worker')`. Should error — name already active. Document the error message.

### T26: Session Not Found Error
Without opening any session, call `slipstream(ops=[...])`. Should get clear "no active session" error. Also try `slipstream(session="nonexistent", ops=[...])`.

### T27: Help Reference Card
Call `slipstream_help()`. Verify it returns formatted text with ops format, session actions, query syntax, and workflows.

### T28: Query List
Open 2 named sessions. `slipstream_query('list')`. Verify both appear. Close one. `slipstream_query('list')` again. Verify only one remains.

---

## Report Format

After all tests, produce a summary table:

```
| Test | Result | Tool Calls | Notes |
|------|--------|------------|-------|
| T1   | PASS   | 5          | 120ms total |
| T2   | PASS   | 4          | 850ms, 30/30 correct |
| T3   | FAIL   | 1          | Error: "resource limit exceeded: max 32 files per session" |
| ...  | ...    | ...        | ... |
```

Then a narrative section:
- **What works well** — strengths, sweet spots
- **What breaks** — failures, unclear errors, edge cases
- **What's missing** — features you wished existed during testing
- **Performance observations** — where it's fast, where it's slow, what scales
- **Recommendations** — prioritized list of improvements

Save the full report to `docs/reports/stress-test-results.md`.
