# H2H Scale Test Prompts

Three contenders, identical task, different tool strategies.
Each agent must setup, verify setup, apply edits, and verify results.

## Task

20 Python microservice files × 3 cross-cutting edits:

1. **Copyright header** — Add `# Copyright 2026 Acme Corp` as line 1 of every `.py` file
2. **Logging setup** — Add `import logging` + `logger = logging.getLogger(__name__)` after existing imports
3. **Rename** — Replace `get_connection` → `acquire_connection` in 11 files that use it

**Setup script**: `python3 {SETUP_SCRIPT} create {WORKDIR}` generates 20 files.
**Verification**: `python3 {SETUP_SCRIPT} verify {WORKDIR}` checks all 60 assertions.

---

## Contender A: Traditional (Read/Edit/Write/Glob/Grep + Bash)

```
You are Contender A in a timed benchmark. You must:

1. SETUP: Run `python3 {SETUP_SCRIPT} create {WORKDIR}` via Bash to generate test files
2. VERIFY SETUP: Confirm 20 .py files exist in {WORKDIR}/services/
3. APPLY EDITS: Apply the 3 edits below to ALL 20 files using Read/Edit/Write/Glob/Grep
4. VERIFY RESULTS: Run `python3 {SETUP_SCRIPT} verify {WORKDIR}` via Bash

Use ONLY Bash, Read, Edit, Write, Glob, and Grep tools. No MCP tools.

Edit 1: Add copyright header
  Add `# Copyright 2026 Acme Corp` as the very first line of every .py file.

Edit 2: Add logging setup
  Add these two lines after the existing imports in every file:
    import logging
    logger = logging.getLogger(__name__)

Edit 3: Rename get_connection → acquire_connection
  Replace every occurrence of `get_connection` with `acquire_connection` in all files
  where it appears: auth_service.py, user_service.py, order_service.py,
  payment_service.py, inventory_service.py, shipping_service.py, health_check.py,
  analytics_service.py, task_queue.py, audit_log.py, db_pool.py.

Work as fast as possible — parallelize tool calls wherever possible.
Report total tool calls and what you did when finished.
```

---

## Contender B1: Slipstream MCP (ss + ss_session + Bash)

```
You are Contender B1 in a timed benchmark. You must:

1. SETUP: Run `python3 {SETUP_SCRIPT} create {WORKDIR}` via Bash to generate test files
2. VERIFY SETUP: Confirm 20 .py files exist in {WORKDIR}/services/
3. APPLY EDITS: Apply the 3 edits below using ss and ss_session MCP tools
4. VERIFY RESULTS: Run `python3 {SETUP_SCRIPT} verify {WORKDIR}` via Bash

Strategy: 3 self-contained ss(ops=[...]) batch calls. No ss_session needed.

Step 1: Batch 1 — Copyright headers (20 file.write ops)
  ss(ops=[...])
  Each op: {"method": "file.write", "path": "...", "start": 0, "end": 0,
            "content": ["# Copyright 2026 Acme Corp"]}

Step 2: Batch 2 — Logging setup (20 str_replace ops)
  ss(ops=[...])
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

Step 3: Batch 3 — Rename (11 replace_all ops)
  ss(ops=[...])
  Each op: {"method": "file.str_replace", "path": "...",
            "old_str": "get_connection", "new_str": "acquire_connection",
            "replace_all": true}
  Files: auth_service.py, user_service.py, order_service.py, payment_service.py,
  inventory_service.py, shipping_service.py, health_check.py, analytics_service.py,
  task_queue.py, audit_log.py, db_pool.py.

Target: 5 tool calls total (setup + 3 batches + verify).
Report total tool calls and what you did when finished.
```

---

## Contender B2: Slipstream CLI (slipstream exec via Bash)

```
You are Contender B2 in a timed benchmark. You must:

1. SETUP: Run `python3 {SETUP_SCRIPT} create {WORKDIR}` via Bash to generate test files
2. VERIFY SETUP: Confirm 20 .py files exist in {WORKDIR}/services/
3. APPLY EDITS: Apply the 3 edits below using `slipstream exec` via Bash
4. VERIFY RESULTS: Run `python3 {SETUP_SCRIPT} verify {WORKDIR}` via Bash

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

All paths must be absolute. Target: 3 Bash calls (setup + edit + verify).
Report total tool calls and what you did when finished.
```

---

## Contender B3: Slipstream MCP + mish (ss + ss_session + mish)

```
You are Contender B3 in a timed benchmark. You must:

1. SETUP: Run `python3 {SETUP_SCRIPT} create {WORKDIR}` via mish sh_run
2. VERIFY SETUP: Confirm 20 .py files exist in {WORKDIR}/services/ via mish sh_run
3. APPLY EDITS: Apply the 3 edits below using ss and ss_session MCP tools
4. VERIFY RESULTS: Run `python3 {SETUP_SCRIPT} verify {WORKDIR}` via mish sh_run

Do NOT use native Bash, Read, Edit, Write, Glob, or Grep tools.
Use ONLY mish (sh_run) and ss (ss, ss_session) tools.

Strategy: 3 self-contained ss(ops=[...]) batch calls. No ss_session needed.

Step 1: Batch 1 — Copyright headers (20 file.write ops)
  ss(ops=[...])
  Each op: {"method": "file.write", "path": "...", "start": 0, "end": 0,
            "content": ["# Copyright 2026 Acme Corp"]}

Step 2: Batch 2 — Logging setup (20 str_replace ops)
  ss(ops=[...])
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

Step 3: Batch 3 — Rename (11 replace_all ops)
  ss(ops=[...])
  Each op: {"method": "file.str_replace", "path": "...",
            "old_str": "get_connection", "new_str": "acquire_connection",
            "replace_all": true}
  Files: auth_service.py, user_service.py, order_service.py, payment_service.py,
  inventory_service.py, shipping_service.py, health_check.py, analytics_service.py,
  task_queue.py, audit_log.py, db_pool.py.

Target: 5 tool calls total (setup + 3 batches + verify).
Report total tool calls and what you did when finished.
```

---

## How to Run

```bash
# Set these before launching:
SETUP_SCRIPT=/Users/scottmeyer/projects/slipstream/docs/benchmarks/h2h-scale/setup.py

# Each contender gets its own workdir:
#   A:  /tmp/h2h-scale-traditional
#   B1: /tmp/h2h-scale-ss-mcp
#   B2: /tmp/h2h-scale-ss-cli
#   B3: /tmp/h2h-scale-ss-mish

# Launch all 4 as parallel subagents (general-purpose, bypassPermissions).
# Each agent handles its own setup and verification.
# Compare: tool calls, wall time, tokens, correctness (60/60).
```
