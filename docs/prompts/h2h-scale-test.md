# Head-to-Head Scale Test: 20 Files × 3 Cross-Cutting Edits

## Context

The first H2H benchmark (5 files, 8 edits) showed 13 vs 2 tool calls but identical wall time (~25s) because LLM thinking dominated. This test scales to 20 files with mechanical edits where round-trip overhead is the bottleneck, not reasoning.

## Setup

Run `benchmark-setup-scale.py create <workdir>` to generate 20 Python microservice files (~40-80 lines each). Then run two subagents in parallel with identical edit instructions.

### Step 1: Create the scale test helper

Create `docs/benchmark-setup-scale.py` that generates 20 files:

```
services/
  auth_service.py
  user_service.py
  order_service.py
  payment_service.py
  inventory_service.py
  shipping_service.py
  notification_service.py
  search_service.py
  analytics_service.py
  cache_service.py
  rate_limiter.py
  health_check.py
  middleware.py
  router.py
  config_loader.py
  db_pool.py
  event_bus.py
  task_queue.py
  file_storage.py
  audit_log.py
```

Each file should:
- Be 40-80 lines of realistic Python (classes, functions, docstrings)
- Import and use `get_connection()` from db_pool in ~12 of the 20 files
- Have NO copyright header
- Have NO `import logging` / logger setup
- Use realistic patterns (dataclasses, context managers, error handling)

The `verify` command checks all 3 edits across all 20 files:
1. Every file has `# Copyright 2026 Acme Corp` as line 1
2. Every file has `import logging` and `logger = logging.getLogger(__name__)`
3. All occurrences of `get_connection` are now `acquire_connection`

### Step 2: Launch both contenders as parallel subagents

**Contender A (Traditional)** — general-purpose agent, model=sonnet, bypassPermissions:
```
You are Contender A in a benchmark. Apply these 3 cross-cutting edits to ALL 20 Python
files in /tmp/h2h-scale-traditional/services/ using ONLY Read and Edit tools.

Edit 1: Add copyright header
  Add `# Copyright 2026 Acme Corp` as the very first line of every .py file.

Edit 2: Add logging setup
  Add these two lines after the existing imports in every file:
    import logging
    logger = logging.getLogger(__name__)

Edit 3: Rename get_connection → acquire_connection
  Replace every occurrence of `get_connection` with `acquire_connection` in all files
  where it appears.

IMPORTANT: Apply all 3 edits to ALL 20 files. Use Read to read each file, Edit to modify.
Work as fast as possible — this is a timed benchmark.
```

**Contender B (Slipstream)** — general-purpose agent, model=sonnet, bypassPermissions:
```
You are Contender B in a benchmark. Apply these 3 cross-cutting edits to ALL 20 Python
files in /tmp/h2h-scale-slipstream/services/ using Slipstream MCP tools.

Use slipstream() one-shot mode: files=[all 20 paths], ops=[...], flush=true.
Use DSL with replace_all for the rename. Use JSON objects for multi-line insertions.
Minimize tool calls — ideally 1-2 total.

Edit 1: Add copyright header
  Add `# Copyright 2026 Acme Corp` as the very first line of every .py file.

Edit 2: Add logging setup
  Add these two lines after the existing imports in every file:
    import logging
    logger = logging.getLogger(__name__)

Edit 3: Rename get_connection → acquire_connection
  Replace every occurrence of `get_connection` with `acquire_connection` in all files
  where it appears.

IMPORTANT: Apply all 3 edits to ALL 20 files. Minimize tool calls. This is a timed benchmark.
```

### Step 3: Verify and compare

```bash
python3 docs/benchmark-setup-scale.py verify /tmp/h2h-scale-traditional
python3 docs/benchmark-setup-scale.py verify /tmp/h2h-scale-slipstream
```

Compare from the agent output:
- Tool calls (from usage metadata)
- Wall time (duration_ms)
- Total tokens
- Correctness (60 checks: 20 copyright + 20 logging + 20 rename)

### Expected results

| Metric | Traditional | Slipstream |
|--------|-------------|------------|
| Tool calls | ~60 (20 reads + 40 edits) | ~2 (1 ToolSearch + 1 slipstream) |
| Wall time | ~90-180s | ~25-35s |
| Correctness | 60/60 | 60/60 |

The gap should be dramatic — Traditional is bottlenecked on serial round-trips
(each Read/Edit is a full LLM turn), while Slipstream batches everything into
one operation.

## What this proves

At small scale (5 files), both approaches finish in similar time because LLM
reasoning dominates. At 20 files with mechanical edits, the tool-call overhead
becomes the bottleneck. Each traditional Read/Edit is a full API round-trip
(1-3s). Slipstream collapses 60 round-trips into 1.
