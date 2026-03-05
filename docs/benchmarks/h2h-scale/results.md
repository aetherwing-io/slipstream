# H2H Scale Benchmark Results

## Run 2 — 2026-03-04 (Post Tool Surface Redesign)

**Model**: Claude Opus 4.6 (subagents via Agent tool)
**Task**: 20 Python files × 3 cross-cutting edits = 60 assertions
**Tool surface**: 3 tools (`ss`, `ss_session`, `ss_help`) — down from 14 → 4 → 3

| Metric | A: Traditional | B1: SS MCP | B2: SS CLI |
|--------|---------------|------------|------------|
| **Tool calls** | 45 | 6 | 1 |
| **Wall time** | 130s | 74s | 43s |
| **Tokens** | 58K | 43K | 40K |
| **Correctness** | 60/60 | 60/60 | 60/60 |

### Contender Strategies

- **A (Traditional)**: Glob → Read all 20 → Write all 20 (all 3 edits per file) → Grep verify. Smart — applied all edits per-file in Write instead of separate Edit calls.
- **B1 (SS MCP)**: open → batch write (headers) → batch str_replace (logging) → batch str_replace+flush (rename) → close. 5 Slipstream calls + 1 verification read.
- **B2 (SS CLI)**: Single `slipstream exec --flush` with 51 ops in a heredoc. One tool call total.

---

## Run 1 — 2026-02-28 (Pre Tool Surface Redesign)

**Model**: Claude Sonnet 4 (headless via `claude -p`)
**Tool surface**: 4 tools (`slipstream`, `slipstream_session`, `slipstream_query`, `slipstream_help`)

| Metric | A: Traditional | B1: SS MCP (one massive batch) |
|--------|---------------|-------------------------------|
| **Tool calls** | 51 | 25 |
| **Wall time** | 57s | 119s |
| **Tokens** | 37K | 45K |
| **Correctness** | 60/60 | 60/60 |

B1 used half the tool calls but took **2× longer** because composing one massive JSON payload generated too many output tokens. This motivated the 3-batch strategy tested in Run 2.

---

## Analysis

### Tool Call Reduction

| Strategy | Run 1 | Run 2 | Change |
|----------|-------|-------|--------|
| Traditional | 51 | 45 | -12% (smarter batching) |
| SS MCP | 25 | 6 | **-76%** (3-batch strategy) |
| SS CLI | N/A | 1 | N/A (new contender) |

### Key Insights

1. **3-batch strategy is the fix for MCP overhead.** Run 1's single massive batch (25 tool calls, 119s) was slower than Traditional because one giant JSON payload = too many output tokens. Splitting into 3 focused batches (headers / logging / rename) keeps each payload small while still batching across all 20 files.

2. **CLI is the theoretical floor.** 1 tool call, 43s. But it requires the LLM to compose ~51 JSON ops in a single Bash heredoc, which is fragile at scale. Good for mechanical edits; less robust for edits requiring file-specific reasoning.

3. **Token efficiency tracks tool call reduction.** 58K → 43K → 40K. Each tool call has fixed overhead (tool description, result parsing). Fewer calls = fewer tokens wasted on ceremony.

4. **Wall time is dominated by output token generation, not I/O.** The Traditional approach's 130s is mostly the LLM generating 20 full Write calls. Slipstream's batches let the daemon do the I/O work, freeing the LLM to generate less text overall.

5. **The Traditional agent got smarter.** Run 2's Traditional agent applied all 3 edits per-file in a single Write (45 calls) instead of Read+Edit per-edit (would have been ~120 calls). This is the best the Traditional approach can do — and it's still 3× slower than CLI.

### When to Use Each Strategy

| Strategy | Best For |
|----------|----------|
| **Traditional** | Small edits (1-3 files), exploratory editing, when you need to read-understand-edit |
| **SS MCP (batches)** | Cross-cutting edits across many files, refactoring, when edits are well-defined |
| **SS CLI** | Fully mechanical edits, CI/CD scripts, subagent workflows with Bash-only access |
