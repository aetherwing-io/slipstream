# Stress Test Retrospective — Run 3

**Date:** 2026-03-04
**Context:** First stress test against the new 4-tool MCP surface (post 14→4 consolidation)

## What Changed Between Runs

### Tool Surface: 14 → 4

| Run 2 (14 tools) | Run 3 (4 tools) |
|---|---|
| `slipstream_open`, `slipstream_close`, `slipstream_flush` | `slipstream_session("open/close/flush ...")` |
| `slipstream_read` | `slipstream_query("read ...")` |
| `slipstream_write`, `slipstream_str_replace` | `slipstream(ops=[{method: "file.write"}, ...])` |
| `slipstream_batch` | `slipstream(ops=[...])` |
| `slipstream_exec` | `slipstream(files=[...], ops=[...])` |
| `slipstream_status`, `slipstream_list`, `slipstream_check` | `slipstream_query("status/list/check build")` |
| `slipstream_register`, `slipstream_unregister` | `slipstream_session("register/unregister ...")` |

### Wire Protocol Impact

The critical architectural change: **the old surface called standalone daemon RPC methods (`file.write`, `file.read`, `file.str_replace`); the new surface routes all ops through the `batch` RPC method.**

The daemon still has both code paths (handler.rs lines 30-34), but the MCP server no longer uses the standalone handlers. Every op goes through `handle_batch`.

## The Real Bug

**`MAX_CONTENT_LINES_PER_WRITE` was enforced in `handle_file_write` (line 450) but not in `handle_batch`'s `BatchOp::Write` arm (line 591).** The consolidation changed the call path from standalone → batch, bypassing the validation.

This is a textbook "consolidation regression" — when you merge N paths into 1, validation that lived in the N individual paths doesn't automatically transfer to the single path. The `MAX_BATCH_OPS` check was already in `handle_batch` because it's a batch-specific concern. But per-op validation (content size limits) was only in the standalone handlers.

### Why Run 2 Passed T8

Run 2's MCP server called `file.write` as a standalone RPC → `handle_file_write` → content size check at line 450 → rejected. Run 3's MCP server calls `batch` with a `Write` op → `handle_batch` → no content size check → accepted. Same daemon, different entry point.

### The Fix

Added upfront validation in `handle_batch` (lines 552-565) that scans all ops for `Write` variants and checks `content.len() > MAX_CONTENT_LINES_PER_WRITE` before entering the session lock. This mirrors the `MAX_BATCH_OPS` check pattern. The standalone `handle_file_write` check remains as defense-in-depth for any future direct callers.

## Test Execution Quality

### What I Got Wrong

**T7 false failure.** I manually typed 257 JSON ops in the tool call and reported a FAIL when they all succeeded. In reality, I typed fewer than 256 ops — hand-counting JSON arrays is unreliable. The `MAX_BATCH_OPS` check was always present and working in `handle_batch`. The CLI test confirmed this immediately.

**Lesson:** When testing limits, use generated fixtures with verified counts (like the CLI `@file` feature), not hand-typed inline JSON. The MCP tool interface makes it tempting to inline everything, but for exact-count tests, external files are more reliable.

**T8 incorrectly skipped.** I claimed "can't pass 60K-line array through MCP tool inline" and skipped it. But the previous run tested it via CLI exec with `--ops @file`. I should have done the same — the test spec says "use CLI exec" is a valid approach. Ironically, this skip masked the actual bug.

**Lesson:** When a test is impractical via one path, use another. The test spec doesn't mandate MCP-only testing. The CLI exec with `@file` for ops is specifically designed for this.

**T17 skip was legitimate.** Session timeout requires a 5-minute wait. The previous run marked it "PASS (partial)" by noting the session was alive during active use, which doesn't actually test the timeout. Both runs effectively skip this — it needs an automated test harness.

### What I Got Right

- **Caught the T8 bug on re-examination.** When the user pointed out the skips were wrong, I ran T8 via CLI and immediately discovered the real regression.
- **All functional tests (T1-T6, T9-T16, T18-T28) were thorough.** Correct fixtures, correct assertions, correct on-disk verification.
- **T10 analysis was accurate.** Shared-buffer merge behavior is correctly identified as by-design, with the nuance that users might expect conflict errors.

## How the 4-Tool Surface Affected Execution

### Advantages

1. **Fewer tool calls per workflow.** T12 (one-shot) does open+read+edit+flush+close in 1 call vs 5 separate calls. This is the marquee benefit.
2. **Named session DSL is intuitive.** `as:agent-a` and `session:agent-a` read naturally. T9 and T28 were easy to author.
3. **Batch ops in session mode work cleanly.** T2's 30-edit batch and T6's 50-op mixed batch both went through a single `slipstream(ops=[...])` call.
4. **Help card is comprehensive.** T27 confirms the reference card covers all ops, actions, queries, and workflows.

### Disadvantages

1. **Inline JSON arrays are verbose and error-prone.** The T7 counting error happened because the ops array was massive inline JSON. With the old surface, `slipstream_read(path=...)` was a simple structured call — no JSON array construction needed.
2. **Harder to test limits via MCP.** The old `slipstream_write(content=[...])` could be called with arbitrary content. The new surface requires embedding the write inside an ops array inside the `slipstream()` tool — an extra layer of nesting.
3. **Error locality is weaker in batch mode.** When a batch op fails, the entire batch returns an error. With standalone tools, each call succeeded or failed independently. For debugging, standalone calls give clearer signal.
4. **Validation gaps are harder to spot.** The consolidation moved from "each tool validates its own inputs" to "the batch handler must validate everything." This is where the T8 bug came from — the batch handler inherited the ops but not the per-op validation.

### Net Assessment

The 4-tool surface is a clear win for production LLM workflows (fewer calls, lower latency, lower token overhead). But it shifts complexity from "many simple tools" to "one complex tool with many modes," which requires more disciplined validation in the batch handler. The stress test exposed exactly this trade-off.

## Recommendations for Future Consolidations

1. **Audit validation parity.** When merging N handlers into 1 batch handler, explicitly list every validation check in each standalone handler and verify the batch path replicates each one. A checklist, not a mental scan.
2. **Keep standalone handlers as test fixtures.** The daemon still has `handle_file_write` (line 31). These can serve as reference implementations and as a secondary test path for direct socket callers.
3. **Use CLI `@file` for limit tests.** The MCP tool interface is for normal operations. Limit testing should use CLI exec with `--ops @file` for precise, reproducible payloads.
4. **Add unit tests for batch-path validation.** The 240 existing tests don't cover "batch with oversized write content." Add targeted tests for each resource limit in the batch path.
