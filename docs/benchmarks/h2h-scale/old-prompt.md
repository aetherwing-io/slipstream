# Head-to-Head Scale Test: 20 Files × 3 Cross-Cutting Edits

## Context

The first H2H scale benchmark showed:
- **Traditional (Read/Edit)**: 51 tool calls, 57s, 37K tokens, 60/60
- **Slipstream MCP (one massive batch)**: 25 tool calls, 119s, 45K tokens, 60/60

Slipstream uses half the tool calls but takes **2× longer** because the LLM must compose one massive JSON payload (~60 ops). More output tokens per call = more wall time.

This round tests whether **smaller batches** or **CLI bypass** can close the gap.

## Setup

Run `benchmark-setup-scale.py create <workdir>` to generate 20 Python microservice files (~40-80 lines each). Then run three subagents in parallel with identical edit instructions but different strategies.

```bash
python3 docs/benchmark-setup-scale.py create /tmp/h2h-scale-traditional
python3 docs/benchmark-setup-scale.py create /tmp/h2h-scale-slipstream-b1
python3 docs/benchmark-setup-scale.py create /tmp/h2h-scale-slipstream-b2
```

## The 3 Edits (same for all contenders)

1. **Copyright header**: Add `# Copyright 2026 Acme Corp` as the very first line of every `.py` file.
2. **Logging setup**: Add `import logging` and `logger = logging.getLogger(__name__)` after existing imports in every file.
3. **Rename**: Replace every occurrence of `get_connection` with `acquire_connection` in all files where it appears.

## Contender A: Traditional (baseline)

general-purpose agent, model=sonnet, bypassPermissions:

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
  where it appears (~11 files use it).

IMPORTANT: Apply all 3 edits to ALL 20 files. Use Read to read each file, Edit to modify.
Work as fast as possible — this is a timed benchmark.
```

## Contender B1: Slipstream Session + 3 Small Batches

Break the work into 3 focused batches (one per edit type) instead of one massive payload.
Each batch is small and mechanical — the LLM doesn't need to reason about all 3 edit types at once.

general-purpose agent, model=sonnet, bypassPermissions:

```
You are Contender B1 in a benchmark. Apply these 3 cross-cutting edits to ALL 20 Python
files in /tmp/h2h-scale-slipstream-b1/services/ using Slipstream MCP tools.

Strategy: Use session mode with 3 small focused batches instead of one massive payload.

Step 1: Open all files in a session
  slipstream_session("open /tmp/h2h-scale-slipstream-b1/services/*.py")

Step 2: Batch 1 — Copyright headers (20 file.write ops)
  slipstream(ops=[...20 ops...])
  Each op as DSL: "write <path> start:0 end:0 content:\"# Copyright 2026 Acme Corp\""
  Or as JSON: {"method": "file.write", "path": "<file>", "start": 0, "end": 0, "content": ["# Copyright 2026 Acme Corp"]}

Step 3: Batch 2 — Logging setup (20 str_replace ops)
  slipstream(ops=[...20 ops...])
  For each file, use str_replace to find the last import line and replace it with
  itself + the logging lines.
  Each op as DSL: "str_replace <path> old:\"<last_import>\" new:\"<last_import>\nimport logging\nlogger = logging.getLogger(__name__)\""
  Or as JSON: {"method": "file.str_replace", "path": "<file>", "old_str": "<last_import>", "new_str": "<last_import>\nimport logging\nlogger = logging.getLogger(__name__)"}

Step 4: Batch 3 — Rename get_connection → acquire_connection (~11 replace_all ops)
  slipstream(ops=[...11 ops...], flush=true)
  Each op as DSL: "str_replace <path> old:\"get_connection\" new:\"acquire_connection\" replace_all"
  Or as JSON: {"method": "file.str_replace", "path": "<file>", "old_str": "get_connection", "new_str": "acquire_connection", "replace_all": true}
  Only include the ~11 files that use get_connection. Include flush=true to write to disk.

Step 5: Close
  slipstream_session("close")

Target: ~5-7 tool calls total. Each payload is small and focused.

IMPORTANT: Apply all 3 edits to ALL 20 files. Minimize tool calls. This is a timed benchmark.

The files that use get_connection are: auth_service.py, user_service.py, order_service.py,
payment_service.py, inventory_service.py, shipping_service.py, health_check.py,
analytics_service.py, task_queue.py, audit_log.py, db_pool.py.
```

## Contender B2: CLI via Bash

Bypass MCP entirely. Use `slipstream exec` with ops piped via stdin heredoc.
Tests whether the MCP protocol layer itself adds overhead.

general-purpose agent, model=sonnet, bypassPermissions:

```
You are Contender B2 in a benchmark. Apply these 3 cross-cutting edits to ALL 20 Python
files in /tmp/h2h-scale-slipstream-b2/services/ using Slipstream CLI (slipstream exec)
via the Bash tool.

Strategy: Use `slipstream exec` with all ops in a single Bash heredoc. One tool call.

The CLI command format:
  slipstream exec \
    --files /tmp/h2h-scale-slipstream-b2/services/*.py \
    --ops @- \
    --flush <<'EOF'
  [JSON array of all operations]
  EOF

Build a JSON array with all operations. You can mix DSL strings and JSON objects.

1. Copyright headers (20 ops) — use DSL strings or JSON:
   DSL: "write <path> start:0 end:0 content:\"# Copyright 2026 Acme Corp\""
   JSON: {"method": "file.write", "path": "<file>", "start": 0, "end": 0, "content": ["# Copyright 2026 Acme Corp"]}
   (one per file)

2. Logging setup (20 ops):
   For each file, use str_replace to find the last import line and replace it with
   itself + "\nimport logging\nlogger = logging.getLogger(__name__)".
   DSL: "str_replace <path> old:\"<last_import>\" new:\"<last_import>\nimport logging\nlogger = logging.getLogger(__name__)\""
   JSON: {"method": "file.str_replace", "path": "<file>", "old_str": "<last_import>", "new_str": "<last_import>\nimport logging\nlogger = logging.getLogger(__name__)"}
   Here are the last import lines:
   - auth_service.py: "from db_pool import get_connection"
   - user_service.py: "from db_pool import get_connection"
   - order_service.py: "from db_pool import get_connection"
   - payment_service.py: "from db_pool import get_connection"
   - inventory_service.py: "from db_pool import get_connection"
   - shipping_service.py: "from db_pool import get_connection"
   - notification_service.py: "from enum import Enum"
   - search_service.py: "from typing import List, Optional, Dict, Any"
   - analytics_service.py: "from db_pool import get_connection"
   - cache_service.py: "import time"
   - rate_limiter.py: "import time"
   - health_check.py: "from db_pool import get_connection"
   - middleware.py: "from typing import Callable, Any, Dict, List"
   - router.py: "import re"
   - config_loader.py: "import json"
   - db_pool.py: "from contextlib import contextmanager"
   - event_bus.py: "from typing import Callable, Dict, List, Any"
   - task_queue.py: "from db_pool import get_connection"
   - file_storage.py: "import hashlib"
   - audit_log.py: "from db_pool import get_connection"

3. Rename get_connection → acquire_connection (11 ops):
   DSL: "str_replace <path> old:\"get_connection\" new:\"acquire_connection\" replace_all"
   JSON: {"method": "file.str_replace", "path": "<file>", "old_str": "get_connection", "new_str": "acquire_connection", "replace_all": true}
   For these files: auth_service.py, user_service.py, order_service.py, payment_service.py,
   inventory_service.py, shipping_service.py, health_check.py, analytics_service.py,
   task_queue.py, audit_log.py, db_pool.py.

IMPORTANT: All paths must be absolute (/tmp/h2h-scale-slipstream-b2/services/filename.py).
Apply all 3 edits to ALL 20 files. Target: 1 Bash tool call. This is a timed benchmark.
```

## Verification

```bash
python3 docs/benchmark-setup-scale.py verify /tmp/h2h-scale-traditional
python3 docs/benchmark-setup-scale.py verify /tmp/h2h-scale-slipstream-b1
python3 docs/benchmark-setup-scale.py verify /tmp/h2h-scale-slipstream-b2
```

## Metrics to Compare

| Metric | A: Traditional | B1: Session+3 Batches | B2: CLI via Bash |
|--------|---------------|----------------------|------------------|
| Tool calls | ~51 | ~5-7 | ~1-2 |
| Wall time | ~57s (baseline) | target: <57s | target: <57s |
| Tokens | ~37K | target: <37K | target: <37K |
| Correctness | 60/60 | 60/60 | 60/60 |

## What this proves

- **A vs B1**: Tests whether breaking a massive payload into 3 small batches reduces LLM generation time enough to beat traditional round-trip overhead.
- **A vs B2**: Tests whether bypassing MCP entirely (CLI via Bash) eliminates protocol overhead.
- **B1 vs B2**: Tests whether MCP adds measurable overhead vs direct CLI invocation.

The hypothesis: B1 should be fastest because it combines low tool calls (5-7) with small per-call payloads. B2 tests the floor — if MCP overhead is negligible, B1 ≈ B2.
