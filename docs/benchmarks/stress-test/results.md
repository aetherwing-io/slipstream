# Slipstream Stress Test Results

**Date:** 2026-03-04 (Run 3 — post MCP consolidation)
**MCP Surface:** 4 tools (slipstream, slipstream_session, slipstream_query, slipstream_help)
**Test Environment:** macOS Darwin 24.6.0, Rust daemon via Unix socket, MCP stdio transport
**Tester:** Claude Opus 4.6 (automated)

## Summary Table

| Test | Result | Tool Calls | Notes |
|------|--------|------------|-------|
| T1: Basic Lifecycle | PASS | 5 | open→read→edit→flush→close, edit verified on disk |
| T2: Multi-File (30) | PASS | 4 | 30 files, 30 str_replaces in 1 batch, 30/30 correct |
| T3: File Limit (33) | PASS | 1 | Error: "too many files in session: 33 (max 32)" [-32003] |
| T4: Large File (~934KB) | PASS | 5 | 15,000 lines, range read + mid-file edit + flush |
| T5: Over 1MB | PASS | 1 | Error: "file too large: toobig.txt is 1248894 bytes (limit: 1048576)" [-32603] |
| T6: Batch 50 ops | PASS | 4 | 10 reads + 40 str_replaces in 1 batch, 40/40 verified |
| T7: Batch Limit (257) | PASS | 1 (CLI) | Error: "too many batch operations: 257 (max 256)" [-32003] |
| T8: Content Size Limit | PASS | 1 (CLI) | Error: "content too large: 60000 lines (max 50000)" [-32003]. **Bug found & fixed**: batch path was missing this check |
| T9: Named Sessions (3) | PASS | 9 | 3 named sessions, 6 files, zero cross-contamination |
| T10: Shared File Conflict | PARTIAL | 8 | Both edits landed (no data loss), but no conflict error on 2nd flush |
| T11: str_replace Ambiguity | PASS | 4 | 3-match: error w/ hint; replace_all: 3 edits; 0-match: error |
| T12: One-Shot Mode | PASS | 1 | 1 tool call: open+read+5 edits+flush+close, 5/5 correct |
| T13: One-Shot No Flush | PASS | 2 | Edit in memory, disk unchanged, session auto-closed |
| T14: Coordinator Status | PASS | 3 | `status` shows dirty; `check build` warns with flush suggestion |
| T15: External Registration | PASS | 4 | register/unregister cycle, tracked as "externally-managed" |
| T16: Invalid Handler | PASS | 1 | Path traversal rejected with clear validation error [-32602] |
| T17: Session Timeout | SKIP | — | Requires 5-min wait; not testable in interactive session |
| T18: Empty File | PASS | 6 | Opens (0 lines), reads [], str_replace errors, file.write works |
| T19: Binary File | PASS | 1 | "file is not valid UTF-8: binary.bin" — clean, no path leak |
| T20: Rapid Cycling (50) | PASS | 2 | 50 CLI open/close cycles, daemon responsive, no leaked sessions |
| T21: Cursor Operations | PASS | 5 | cursor.move + count-based reads, cursor tracking correct |
| T22: CLI exec | PASS | 1 | read+edit+flush via `slipstream exec`, verified on disk |
| T23: Parallel CLI (3) | PASS | 1 | 3 concurrent CLI exec, all correct, no corruption |
| T24: Default Session | PASS | 5 | Implicit default works without `as:` or `session:` qualifiers |
| T25: Name Collision | PASS | 2 | "session 'worker' already active. Close it first or use a different name." |
| T26: Session Not Found | PASS | 2 | Clear errors with actionable suggestions for both cases |
| T27: Help Card | PASS | 1 | Full reference: ops, sessions, queries, 4 workflows |
| T28: Query List | PASS | 4 | Accurate session listing, updates correctly after close |

**Results: 26 PASS, 1 PARTIAL, 0 FAIL, 1 SKIP**

---

## Bug Found & Fixed: MAX_CONTENT_LINES_PER_WRITE Not Enforced in Batch Path

The standalone `handle_file_write` handler (line 450) checked `MAX_CONTENT_LINES_PER_WRITE`, but the `handle_batch` handler processed `BatchOp::Write` ops without checking the limit. Since all MCP tool calls route through `handle_batch`, the 50,000-line limit was never enforced via the MCP surface.

**Root cause:** The batch handler was missing the content size validation that the standalone handler had.

**Fix:** Added upfront validation in `handle_batch` (handler.rs:552-565) that scans all ops for `Write` variants and checks content length before entering the session lock. This matches the pattern used for `MAX_BATCH_OPS` (line 542).

**T7 clarification:** The `MAX_BATCH_OPS=256` check was always enforced — it lives in `handle_batch` at line 542. The initial T7 "fail" was a test counting error (manually typed ops array was likely <256). CLI verification confirms the limit works correctly.

---

## Narrative

### What Works Well

- **One-shot mode is the killer feature.** T12: 1 tool call replaces 7+ traditional Read/Edit calls. Open 5 files, read all, apply 5 edits, flush, close — all atomic.
- **Batch operations scale cleanly.** 30 files × 1 edit (T2), 50 mixed ops across 5 files (T6) — all handled in single batches with zero errors.
- **Named sessions provide real isolation.** T9: 3 concurrent named sessions with different files showed zero cross-contamination.
- **Error messages are excellent.** Every limit hit (T3, T5, T7, T8, T11, T16, T19, T25, T26) produced clear, actionable messages with specific values and suggestions.
- **Coordinator status is genuinely useful.** T14's `check build` providing specific flush suggestions with session IDs is exactly what an LLM agent needs.
- **CLI exec path is solid.** T22 and T23 confirm subagents can use Slipstream via Bash, including parallel execution with no contention.
- **Cursor tracking works correctly.** T21 verified cursor state across moves and count-based reads.
- **Security validation works.** Path traversal in handler names rejected (T16). Binary files show only filename (T19).
- **Resource cleanup is clean.** 50 rapid open/close cycles (T20) left no leaked sessions.

### What Breaks

- **Shared file conflict detection is too permissive (T10).** Two sessions sharing a file can both flush without conflict if they edit different regions. By design (shared buffers), but users may expect a conflict error.

### What's Missing

- **file.write parameter discoverability.** `start` and `end` are required but error messages aren't helpful for discovery.
- **Session timeout not configurable.** The 5-min timeout is hardcoded.
- **Named session names not in `list` output.** List shows session IDs but not human-readable names.
- **No `session.add_files` action.** Can't add files to an existing session without close/reopen.

### Performance Observations

- **Small file operations are instant.** Single-file lifecycle (T1) completes in well under a second.
- **30-file batch (T2)** completes in 4 tool calls — dramatically fewer than 30 individual Read/Edit cycles.
- **934KB file (T4)** — 15K lines with range read + mid-file edit showed no latency degradation.
- **50-op batch (T6)** — processed in a single tool call without issues.
- **50 rapid CLI cycles (T20)** — no resource leaks detected.
- **3 parallel CLI execs (T23)** — no locking contention.

### Recommendations (Prioritized)

1. ~~**BUG: Fix MAX_CONTENT_LINES_PER_WRITE in batch path.**~~ **FIXED** — added upfront validation in `handle_batch`.
2. **ENHANCEMENT: Show session names in `list` output.** Map human-readable names alongside session UUIDs.
3. **ENHANCEMENT: Add `session.add_files` action.** Allow adding files to an existing session without close/reopen.
4. **DOCS: Improve file.write parameter documentation.** Add explicit examples showing required `start`/`end` fields.
5. **CONSIDER: Configurable conflict strictness.** A `--strict` flag on flush for non-overlapping region conflicts.
6. **CONSIDER: Configurable session timeout.** Per-session or global timeout override.
