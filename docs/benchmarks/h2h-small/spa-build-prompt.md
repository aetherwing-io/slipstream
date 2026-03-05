# H2H Benchmark: Build a Working SPA

## Context

Previous benchmarks used synthetic edits (add headers, rename functions). This test measures real-world productivity: **build a working single-page app from scratch**. Both contenders get identical specs, identical model, identical budget. We measure correctness, tool calls, wall time, and tokens.

## The App: Task Tracker SPA

A self-contained single-file HTML/CSS/JS task tracker with:

1. **Add tasks** — text input + "Add" button, Enter key submits
2. **Complete tasks** — click to toggle strikethrough + opacity change
3. **Delete tasks** — "×" button per task, with confirmation (double-click or hold)
4. **Filter** — "All / Active / Completed" toggle buttons, highlighted current filter
5. **Counter** — "N items left" live count (active only)
6. **Persist** — localStorage, survives page refresh
7. **Bulk actions** — "Clear completed" button (only visible when completed tasks exist)
8. **Empty state** — show "No tasks yet" when list is empty
9. **Styling** — clean, centered layout, max-width 600px, subtle shadows, smooth transitions on complete/delete
10. **Keyboard** — Escape clears input, focus input on page load

All in a single `index.html` file. No frameworks, no build tools, no CDN dependencies.

## Setup

```bash
# From a regular terminal (not inside Claude Code):
mkdir -p /tmp/h2h-spa-traditional /tmp/h2h-spa-slipstream
```

## Launch: Two Parallel Subagents

### Contender A: Traditional (Read/Edit/Write)

```
subagent_type: general-purpose
model: sonnet
mode: bypassPermissions
allowedTools: Read, Edit, Write, Glob, Grep, Bash
```

**Prompt:**
```
Build a task tracker single-page app. Create ONE file: /tmp/h2h-spa-traditional/index.html

Requirements:
1. Add tasks — text input + "Add" button, Enter key submits
2. Complete tasks — click to toggle strikethrough + opacity change
3. Delete tasks — "×" button per task
4. Filter — "All / Active / Completed" toggle buttons, highlighted current filter
5. Counter — "N items left" live count (active only)
6. Persist — localStorage, survives page refresh
7. Bulk actions — "Clear completed" button (only visible when completed tasks exist)
8. Empty state — show "No tasks yet" when list is empty
9. Styling — clean, centered layout, max-width 600px, subtle shadows, smooth transitions
10. Keyboard — Escape clears input, focus input on page load

Single index.html file. No frameworks, no CDN. Pure HTML/CSS/JS.

After creating the file, verify it by reading it back and checking all 10 requirements are addressed in the code.
```

### Contender B: Slipstream + mish

```
subagent_type: general-purpose
model: sonnet
mode: bypassPermissions
allowedTools: Read, Edit, Write, Glob, Grep, Bash, mcp__slipstream__slipstream, mcp__slipstream__slipstream_session, mcp__slipstream__slipstream_query, mcp__slipstream__slipstream_help, mcp__mish__sh_run, mcp__mish__sh_spawn, mcp__mish__sh_interact, mcp__mish__sh_session, mcp__mish__sh_help
```

**Prompt:**
```
Build a task tracker single-page app. Create ONE file: /tmp/h2h-spa-slipstream/index.html

Requirements:
1. Add tasks — text input + "Add" button, Enter key submits
2. Complete tasks — click to toggle strikethrough + opacity change
3. Delete tasks — "×" button per task
4. Filter — "All / Active / Completed" toggle buttons, highlighted current filter
5. Counter — "N items left" live count (active only)
6. Persist — localStorage, survives page refresh
7. Bulk actions — "Clear completed" button (only visible when completed tasks exist)
8. Empty state — show "No tasks yet" when list is empty
9. Styling — clean, centered layout, max-width 600px, subtle shadows, smooth transitions
10. Keyboard — Escape clears input, focus input on page load

Single index.html file. No frameworks, no CDN. Pure HTML/CSS/JS.

You have Slipstream MCP tools for file editing and mish for running shell commands.
Use slipstream one-shot mode to create and edit files efficiently.
Use mish to run any verification commands (e.g., checking file size, grepping for features).

After creating the file, verify it by reading it back and checking all 10 requirements are addressed in the code.
```

## Verification Script

Save as `docs/benchmark-verify-spa.py`:

```python
#!/usr/bin/env python3
"""Verify SPA task tracker implementation."""
import sys
import os
import re

def verify(html_path):
    if not os.path.exists(html_path):
        print(f"MISSING: {html_path}")
        return False

    with open(html_path) as f:
        content = f.read()

    checks = {
        "has_input": bool(re.search(r'<input[^>]*type=["\']text["\']', content) or re.search(r'<input[^>]*placeholder', content)),
        "has_add_button": bool(re.search(r'(?i)add|submit', content) and re.search(r'<button', content)),
        "enter_key": bool(re.search(r'keydown|keypress|keyup|Enter|enter', content)),
        "toggle_complete": bool(re.search(r'strikethrough|line-through|completed|toggle', content)),
        "delete_button": bool(re.search(r'×|&times;|delete|remove', content, re.IGNORECASE)),
        "filter_buttons": bool(re.search(r'(?i)all.*active.*completed|filter', content)),
        "counter": bool(re.search(r'items?\s*left|remaining|count', content, re.IGNORECASE)),
        "localStorage": bool(re.search(r'localStorage', content)),
        "clear_completed": bool(re.search(r'(?i)clear\s*completed', content)),
        "empty_state": bool(re.search(r'(?i)no\s*tasks|empty', content)),
        "centered_layout": bool(re.search(r'max-width.*600|margin.*auto|text-align.*center', content)),
        "transitions": bool(re.search(r'transition|animation|transform', content)),
        "escape_key": bool(re.search(r'Escape|escape|Esc', content)),
        "autofocus": bool(re.search(r'autofocus|\.focus\(\)', content)),
        "is_single_file": not re.search(r'<link[^>]*href=["\'](?!#)', content),  # no external CSS
        "no_cdn": not re.search(r'https?://cdn|unpkg|jsdelivr|cloudflare', content),
    }

    passed = sum(1 for v in checks.values() if v)
    total = len(checks)

    for name, ok in checks.items():
        status = "PASS" if ok else "FAIL"
        print(f"  {status}: {name}")

    print(f"\nResults: {passed}/{total} passed")
    return passed == total

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: benchmark-verify-spa.py <path-to-index.html>")
        sys.exit(1)
    success = verify(sys.argv[1])
    sys.exit(0 if success else 1)
```

## Running the Benchmark

```python
# Inside Claude Code — launch both contenders as parallel subagents:

# 1. Create fixtures
#    mkdir -p /tmp/h2h-spa-traditional /tmp/h2h-spa-slipstream

# 2. Launch both agents (see prompts above) with run_in_background=true

# 3. When both complete, verify:
#    python3 docs/benchmark-verify-spa.py /tmp/h2h-spa-traditional/index.html
#    python3 docs/benchmark-verify-spa.py /tmp/h2h-spa-slipstream/index.html

# 4. Compare from agent results:
#    - Tool calls (from usage.tool_uses)
#    - Wall time (from usage.duration_ms)
#    - Tokens (from usage.total_tokens)
#    - Correctness (from verify script)
```

## What This Measures

Unlike synthetic benchmarks, this tests **real development workflow**:
- File creation (Write/slipstream)
- Iterative editing (Edit/str_replace) — fixing bugs, refining CSS, adding features
- Verification loops (Read back, grep for features)
- The full create→edit→verify cycle that real coding sessions follow

The hypothesis: Slipstream + mish won't show dramatic tool-call reduction for greenfield SPA creation (Write dominates), but **will** show advantage on the iterative edit-verify cycles. Traditional agents can't batch "fix 3 CSS issues + verify" into one call.

## Expected Results

| Metric | Traditional | Slipstream + mish |
|--------|-------------|-------------------|
| Tool calls | ~5-15 | ~3-8 |
| Wall time | ~30-60s | ~25-45s |
| Correctness | 14-16/16 | 14-16/16 |

The gap may be small for greenfield creation. The real test is: **after initial creation, give both agents a list of 5 fixes** (CSS tweaks, add a feature, fix a bug). That's where batch editing shines. Consider a Phase 2 prompt for iteration.
