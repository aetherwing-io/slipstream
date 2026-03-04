# Slipstream Workflow Test Results

**Date**: 2026-03-04
**Tester**: Claude Opus 4.6 via MCP
**Daemon**: slipstream v0.1 (release build, 240 unit tests passing)

## Executive Summary

17 tests across 5 categories. **All 17 PASS**. Key findings:

1. **str_replace uses full-line matching** — not substring. Cross-cutting renames require providing every unique complete line, not just the word to change. `replace_all` helps for repeated identical lines.
2. **Batches are atomic** — if any op fails, all previous ops in the batch are rolled back. Session remains clean and operational.
3. **Shared buffer prevents stale writes** — concurrent sessions share `Arc<RwLock<FileBuffer>>`, so stale edits fail immediately at str_replace time, not at flush time. This is structurally better than flush-time conflict detection.
4. **Error messages lack provenance** — batch failures say "no match found" but don't identify which op in the batch failed. This is the #1 ergonomic issue.
5. **One-shot mode is the killer feature** — 1 tool call for open+edit+flush+close across N files, vs 2N calls for traditional Read/Edit.

---

## Category 1: Real Editing Workflows

### W1: Single-File Refactor
**Result**: PASS
**Task**: Rename `dispatch_op` → `execute_operation` in handler.rs (7 occurrences)
**Tool calls**: 7 (one per occurrence, one-shot mode each)
**Findings**:
- str_replace requires full-line matching. `unreachable!("dispatch_op...")` without leading whitespace (16 spaces) failed; with leading whitespace, it matched.
- When a batch of 7 ops fails, the error message doesn't say which op failed.
- Optimal approach: use multi-line context to disambiguate identical patterns (e.g., `dispatch_op(session, op, mgr)` appeared 4 times with identical content).

### W2: Multi-File Cross-Cutting Edit
**Result**: PASS
**Task**: Rename `SessionId` → `SessId` across 5 files (50 occurrences, 17 unique line patterns)
**Tool calls**: 5 (one per file, one-shot mode)
**Findings**:
- Full-line matching means you can't do a simple word-level find-and-replace. Each unique line pattern needs its own str_replace op.
- `replace_all: true` works well for repeated identical lines (e.g., 8 copies of `    pub session_id: SessionId,` in types.rs).
- 50 occurrences needed 37 ops across 5 calls. Traditional Read/Edit would need ~55 calls. Slipstream wins significantly.

### W3: Read-Understand-Edit Cycle
**Result**: PASS
**Task**: Read dispatch_op, add a new `Op::Delete` match arm based on the pattern of existing arms
**Tool calls**: 4 (open, read, str_replace+flush, close)
**Findings**:
- Read output provides full line content and cursor position — sufficient context for pattern-based edits.
- Error on reading a file not in the session is clear: "file not in session: /path".
- Session mode (open → read → edit → flush → close) is natural for exploratory editing.

### W4: Large Batch Coherence
**Result**: PASS
**Task**: 22 edits across 10 real source files in a single one-shot batch
**Tool calls**: 1
**Findings**:
- All 22 edits across 10 files in 1 tool call. Verified 22 markers on disk, no corruption.
- Failed 3 times before succeeding due to: wrong `pub` visibility on a function name, partial line match instead of full line, and accumulated partial edits from failed batches in session mode.
- **Lesson**: one-shot mode is more reliable than session mode for large batches because each attempt starts clean. Session mode accumulates state from failed batches.

---

## Category 2: Error Recovery

### E1: Wrong str_replace Target
**Result**: PASS
**Task**: Attempt str_replace with `dispatch_op (` (extra space) instead of `dispatch_op(`
**Recovery cost**: 3 tool calls (fail → read → retry)
**Findings**:
- Error message: `str_replace error: no match found for old_str` — clear but could be improved with "did you mean?" suggestions.
- Recovery requires reading the file to see the actual content, then retrying with exact text.

### E2: Partial Batch Failure
**Result**: PASS
**Task**: Batch of 5 ops where op 3 has a nonexistent old_str
**Findings**:
- **Batch is atomic**: ops 1-2 were NOT applied when op 3 failed. Buffer shows original values.
- Session remains clean (`dirty_count: 0`, state: `Clean`).
- Recovery: fix the failing op and retry the entire batch. No partial state to clean up.

### E3: Flush-Then-Regret
**Result**: PASS
**Task**: Edit a value to wrong number, flush, then fix
**Recovery cost**: 1 tool call (str_replace+flush)
**Findings**:
- Session stays active after flush — no need to re-open the file.
- Recovery is simply: str_replace the wrong content with correct content, flush again.
- This is the cheapest recovery path of any editing tool.

### E4: Forgot to Flush
**Result**: PASS
**Task**: Edit 3 files, close session without flushing
**Findings**:
- Disk unchanged — edits are lost on close without flush.
- `check build` correctly warned about 3 files with unflushed edits and provided session IDs.
- This is the safety net: `check build` before any build catches the "forgot to flush" footgun.

---

## Category 3: LLM-Specific Failure Modes

### L1: Whitespace Sensitivity
**Result**: PASS (with critical findings)
**Findings**:
- **Tabs vs spaces**: `    let z` (4 spaces) ≠ `\tlet z` (tab). Full-line exact match. LLMs routinely get this wrong.
- **Trailing whitespace**: `let a = 1;` ≠ `let a = 1;   ` (trailing spaces). The read output preserves trailing whitespace faithfully, so copying from read output works. Guessing from memory fails.
- **Recovery path**: Always read first, then match exactly from the read output.

### L2: Indentation Preservation
**Result**: PASS
**Task**: file.write to replace a code block inside an `if` statement
**Findings**:
- file.write does pure line replacement — no auto-indentation.
- Content array is written exactly as provided. Indentation responsibility is entirely on the caller.
- This is correct behavior — a "smart" auto-indent would be unpredictable and harder to reason about.

### L3: Unicode and Special Characters
**Result**: PASS
**Task**: str_replace in a file with accented chars (é, ö), emoji (🎉🚀), CJK (こんにちは), and math symbols (∑∏∫)
**Findings**:
- All unicode preserved through str_replace and flush. No mojibake.
- Added emoji (✨) and accented text (édited) — both survived round-trip.

### L4: Near-Duplicate Lines
**Result**: PASS
**Task**: Edit `item_2` in a file with `item_1`, `item_2`, `item_3` (near-duplicates) and 3 identical `process_item(item)` lines (true duplicates)
**Findings**:
- Near-duplicates: full-line matching handles naturally — each line is unique due to the number suffix.
- True duplicates: excellent error message — `found 3 matches for old_str (expected exactly 1, include more context to disambiguate or set replace_all)`.
- Adding one surrounding line for context resolves ambiguity immediately.
- Recovery: 2 tool calls (fail + disambiguated retry).

---

## Category 4: Concurrency

### C1: Two Agents, Different Files
**Result**: PASS
**Task**: Named sessions `agent-a` (edit.rs) and `agent-b` (buffer.rs) editing concurrently
**Findings**:
- Clean isolation. No cross-contamination between sessions.
- Both flushed independently, both edits verified on disk.

### C2: Two Agents, Same File, Different Regions
**Result**: PASS
**Task**: `agent-top` edits line 2 (region A), `agent-bottom` edits line 43 (region B) of same file
**Findings**:
- Both edits landed on disk correctly. Non-overlapping regions merge cleanly.
- agent-top's flush returned a **warning** about agent-bottom's pending edit on the same file — excellent conflict awareness.
- agent-bottom's flush succeeded without issues.

### C3: Stale Read — Conflict Detection
**Result**: PASS
**Task**: sess-a reads file, sess-b edits and flushes same line, sess-a tries to edit based on stale read
**Findings**:
- **Race condition is structurally prevented**, not just detected.
- Both sessions share the same `Arc<RwLock<FileBuffer>>`. When sess-b changes `max_retries: 3` → `5`, the shared buffer updates immediately.
- When sess-a tries str_replace with stale `max_retries: 3`, it gets "no match found" because the buffer already shows `5`.
- This is better than flush-time conflict detection — the LLM discovers the stale state immediately and can re-read before retrying.

---

## Category 5: Comparative Benchmarks

### B1: Tool Call Count
**Task**: Add a comment header to 5 files

| Mode | Tool Calls | Notes |
|------|-----------|-------|
| **Slipstream one-shot** | **1** | open+edit+flush+close in single call |
| Slipstream session | 4 | open → batch → flush → close |
| Traditional Read/Edit | 10 | 5 × Read + 5 × Edit |

**Slipstream one-shot is 10x fewer tool calls** than traditional Read/Edit for the same task.

### B2: Recovery Cost
**Task**: Same edit with an intentional error, then fix

| Mode | Error | Recovery | Total |
|------|-------|----------|-------|
| **Slipstream one-shot** | 1 (fail) | 1 (retry) | **2** |
| Traditional Read/Edit | 1 (fail) | 2 (Read + Edit) | **3** |

Recovery is cheaper in slipstream because one-shot opens the file fresh — no separate Read call needed.

**Warning**: `read_all=true` on large files (~1500+ lines) produces 100K+ character output that exceeds MCP result limits. Avoid `read_all` on large files; use targeted reads with `start`/`end` parameters instead.

---

## What I Learned

### The Good
1. **One-shot mode is remarkably efficient** — 1 tool call for multi-file editing is a genuine 10x improvement over traditional workflows.
2. **Atomic batches are the right default** — failed ops don't leave partial state. The LLM can simply fix and retry.
3. **Shared buffer prevents data loss** — stale edits fail at str_replace time, not at flush time. No silent overwrites.
4. **`check build` catches the flush footgun** — the most dangerous LLM error (forgetting to flush) is detected before build.
5. **Unicode works perfectly** — no special handling needed.
6. **Conflict warnings on flush** — when two sessions share a file, the flush warns about the other session's pending edits.

### The Painful
1. **Full-line matching is the #1 source of failures** — LLMs naturally want to do word-level replacements. Every `str_replace` requires the complete line(s). This caused 5+ failed attempts during testing.
2. **No error provenance in batches** — "no match found for old_str" doesn't say which op in a 22-op batch failed. This forced binary-search debugging.
3. **Trailing/leading whitespace sensitivity** — LLMs frequently omit leading spaces or don't know about trailing spaces. The fix is always "read first, then match exactly," but this adds a round-trip.
4. **`read_all` on large files is a token bomb** — handler.rs (1455 lines) as `read_all` output exceeds MCP result limits. Need targeted reads for large files.

### Design Recommendations
1. **Add op index to error messages** — `str_replace error on op 4 of 22: no match found for old_str` would eliminate batch debugging.
2. **Consider fuzzy match suggestions** — when "no match found," show the closest matching line (Levenshtein distance) to help LLMs self-correct.
3. **Add `read_all` line limit** — cap `read_all` output at e.g., 500 lines total, or auto-truncate with a warning, to prevent token bombs.
4. **Document the full-line matching contract prominently** — this is the single most surprising behavior for new users.
