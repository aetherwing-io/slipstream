# H2H Scale Test Prompts

Three contenders, identical task, different tool strategies.

## Task

20 Python microservice files × 3 cross-cutting edits:

1. **Copyright header** — Add `# Copyright 2026 Acme Corp` as line 1 of every `.py` file
2. **Logging setup** — Add `import logging` + `logger = logging.getLogger(__name__)` after existing imports
3. **Rename** — Replace `get_connection` → `acquire_connection` in 11 files that use it

**Verification**: `python3 setup.py verify <workdir>` checks all 60 assertions (20 files × 3 edits).

---

## Contender A: Traditional (Read/Edit/Write/Glob/Grep)

```
You are Contender A in a timed benchmark. Apply these 3 cross-cutting edits to ALL 20
Python files in {WORKDIR}/services/ using ONLY Read, Edit, Write, Glob, and Grep tools.
Do NOT use any MCP tools or Bash.

Edit 1: Add copyright header
  Add `# Copyright 2026 Acme Corp` as the very first line of every .py file.

Edit 2: Add logging setup
  Add these two lines after the existing imports in every file:
    import logging
    logger = logging.getLogger(__name__)

Edit 3: Rename get_connection → acquire_connection
  Replace every occurrence of `get_connection` with `acquire_connection` in all files
  where it appears. The files that use get_connection are: auth_service.py,
  user_service.py, order_service.py, payment_service.py, inventory_service.py,
  shipping_service.py, health_check.py, analytics_service.py, task_queue.py,
  audit_log.py, db_pool.py.

IMPORTANT: Apply all 3 edits to ALL 20 files. Work as fast as possible — this is a
timed benchmark. Parallelize tool calls wherever possible.
```

---

## Contender B1: Slipstream MCP (ss + ss_session)

```
You are Contender B1 in a timed benchmark. Apply these 3 cross-cutting edits to ALL 20
Python files in {WORKDIR}/services/ using Slipstream MCP tools (ss, ss_session).

Strategy: Use session mode with 3 small focused batches.

Step 1: Open all files
  ss_session("open {WORKDIR}/services/*.py")

Step 2: Batch 1 — Copyright headers (20 file.write ops)
  ss(ops=[...], flush=false)
  Each op: {"method": "file.write", "path": "...", "start": 0, "end": 0,
            "content": ["# Copyright 2026 Acme Corp"]}

Step 3: Batch 2 — Logging setup (20 str_replace ops)
  ss(ops=[...], flush=false)
  Each op: {"method": "file.str_replace", "path": "...",
            "old_str": "<last_import>",
            "new_str": "<last_import>\nimport logging\nlogger = logging.getLogger(__name__)"}

  Last import lines per file:
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

Step 4: Batch 3 — Rename (11 replace_all ops, flush=true)
  ss(ops=[...], flush=true)
  Each op: {"method": "file.str_replace", "path": "...",
            "old_str": "get_connection", "new_str": "acquire_connection",
            "replace_all": true}
  Files: auth_service.py, user_service.py, order_service.py, payment_service.py,
  inventory_service.py, shipping_service.py, health_check.py, analytics_service.py,
  task_queue.py, audit_log.py, db_pool.py.

Step 5: Close
  ss_session("close")

Target: ~5-7 tool calls total.
```

---

## Contender B2: Slipstream CLI (slipstream exec via Bash)

```
You are Contender B2 in a timed benchmark. Apply these 3 cross-cutting edits to ALL 20
Python files in {WORKDIR}/services/ using Slipstream CLI (`slipstream exec`) via Bash.

Strategy: Single Bash call with all 51 ops in a heredoc.

slipstream exec \
  --files {WORKDIR}/services/*.py \
  --ops @- \
  --flush <<'EOF'
[
  ... 20 file.write ops for copyright headers ...
  ... 20 file.str_replace ops for logging setup ...
  ... 11 file.str_replace replace_all ops for rename ...
]
EOF

Op formats:
- Write: {"method": "file.write", "path": "...", "start": 0, "end": 0,
          "content": ["# Copyright 2026 Acme Corp"]}
- Logging: {"method": "file.str_replace", "path": "...",
            "old_str": "<last_import>",
            "new_str": "<last_import>\nimport logging\nlogger = logging.getLogger(__name__)"}
- Rename: {"method": "file.str_replace", "path": "...",
           "old_str": "get_connection", "new_str": "acquire_connection",
           "replace_all": true}

All paths must be absolute. Target: 1 Bash tool call.
```

---

## How to Run

```bash
# Create test directories
python3 setup.py create /tmp/h2h-scale-traditional
python3 setup.py create /tmp/h2h-scale-ss-mcp
python3 setup.py create /tmp/h2h-scale-ss-cli

# Launch 3 subagents in parallel (from Claude Code):
# - Contender A: general-purpose, bypassPermissions, tools: Read/Edit/Write/Glob/Grep
# - Contender B1: general-purpose, bypassPermissions, tools: all (needs ss MCP)
# - Contender B2: general-purpose, bypassPermissions, tools: all (needs Bash + slipstream CLI)

# Verify all three
python3 setup.py verify /tmp/h2h-scale-traditional
python3 setup.py verify /tmp/h2h-scale-ss-mcp
python3 setup.py verify /tmp/h2h-scale-ss-cli
```
