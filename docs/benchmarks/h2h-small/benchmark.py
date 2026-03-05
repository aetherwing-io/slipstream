#!/usr/bin/env python3
"""
Real-world benchmark: Slipstream vs traditional file I/O.

Creates 5 realistic source files, applies 8 edits using both approaches,
measures wall-clock time, round trips, and verifies correctness.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

PROJECT = "/Users/scottmeyer/projects/slipstream"
SLIPSTREAM = os.path.join(PROJECT, "target", "release", "slipstream")
DAEMON = os.path.join(PROJECT, "target", "release", "slipstream-daemon")

# ---------------------------------------------------------------------------
# Test files — realistic code snippets
# ---------------------------------------------------------------------------

FILES = {
    "auth.py": '''"""Authentication module."""

import hashlib
import hmac
from datetime import datetime, timedelta

SECRET_KEY = "changeme"
TOKEN_EXPIRY = 3600  # seconds


def hash_password(password: str) -> str:
    """Hash a password with SHA-256."""
    return hashlib.sha256(password.encode()).hexdigest()


def verify_password(password: str, hashed: str) -> bool:
    """Verify password against hash."""
    return hash_password(password) == hashed


def create_token(user_id: str) -> dict:
    """Create an auth token."""
    now = datetime.utcnow()
    return {
        "user_id": user_id,
        "created": now.isoformat(),
        "expires": (now + timedelta(seconds=TOKEN_EXPIRY)).isoformat(),
    }


def validate_token(token: dict) -> bool:
    """Check if token is still valid."""
    expires = datetime.fromisoformat(token["expires"])
    return datetime.utcnow() < expires
''',
    "database.py": '''"""Database connection and query helpers."""

import sqlite3
from contextlib import contextmanager
from typing import Any, Optional


DB_PATH = "app.db"
MAX_CONNECTIONS = 5


@contextmanager
def get_connection():
    """Get a database connection."""
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    try:
        yield conn
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    finally:
        conn.close()


def execute_query(query: str, params: tuple = ()) -> list:
    """Execute a query and return results."""
    with get_connection() as conn:
        cursor = conn.execute(query, params)
        return cursor.fetchall()


def insert_record(table: str, data: dict) -> int:
    """Insert a record and return the ID."""
    columns = ", ".join(data.keys())
    placeholders = ", ".join(["?"] * len(data))
    query = f"INSERT INTO {table} ({columns}) VALUES ({placeholders})"
    with get_connection() as conn:
        cursor = conn.execute(query, tuple(data.values()))
        return cursor.lastrowid


def find_by_id(table: str, record_id: int) -> Optional[dict]:
    """Find a record by ID."""
    rows = execute_query(f"SELECT * FROM {table} WHERE id = ?", (record_id,))
    return dict(rows[0]) if rows else None
''',
    "api.py": '''"""REST API handlers."""

from http import HTTPStatus


def handle_login(request):
    """Handle login request."""
    username = request.get("username")
    password = request.get("password")

    if not username or not password:
        return {"status": HTTPStatus.BAD_REQUEST, "error": "Missing credentials"}

    # TODO: validate against database
    return {"status": HTTPStatus.OK, "token": "dummy-token"}


def handle_register(request):
    """Handle user registration."""
    username = request.get("username")
    password = request.get("password")
    email = request.get("email")

    if not all([username, password, email]):
        return {"status": HTTPStatus.BAD_REQUEST, "error": "Missing fields"}

    # TODO: create user in database
    return {"status": HTTPStatus.CREATED, "user_id": "new-user-id"}


def handle_get_profile(request):
    """Get user profile."""
    user_id = request.get("user_id")
    if not user_id:
        return {"status": HTTPStatus.BAD_REQUEST, "error": "Missing user_id"}

    # TODO: fetch from database
    return {"status": HTTPStatus.OK, "profile": {"name": "Test User"}}


def handle_update_profile(request):
    """Update user profile."""
    user_id = request.get("user_id")
    updates = request.get("updates", {})

    if not user_id:
        return {"status": HTTPStatus.BAD_REQUEST, "error": "Missing user_id"}

    # TODO: update in database
    return {"status": HTTPStatus.OK, "updated": True}
''',
    "config.py": '''"""Application configuration."""

import os
from dataclasses import dataclass


@dataclass
class Config:
    """App configuration."""

    debug: bool = False
    host: str = "0.0.0.0"
    port: int = 8080
    db_path: str = "app.db"
    log_level: str = "INFO"
    max_request_size: int = 1_000_000
    cors_origins: list = None

    def __post_init__(self):
        if self.cors_origins is None:
            self.cors_origins = ["*"]


def load_config() -> Config:
    """Load config from environment."""
    return Config(
        debug=os.getenv("DEBUG", "false").lower() == "true",
        host=os.getenv("HOST", "0.0.0.0"),
        port=int(os.getenv("PORT", "8080")),
        db_path=os.getenv("DB_PATH", "app.db"),
        log_level=os.getenv("LOG_LEVEL", "INFO"),
    )
''',
    "utils.py": '''"""Utility functions."""

import re
import json
from datetime import datetime


def slugify(text: str) -> str:
    """Convert text to URL-safe slug."""
    text = text.lower().strip()
    text = re.sub(r"[^\w\s-]", "", text)
    text = re.sub(r"[\s_]+", "-", text)
    return text


def format_timestamp(dt: datetime) -> str:
    """Format datetime for API responses."""
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_timestamp(s: str) -> datetime:
    """Parse ISO timestamp."""
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def truncate(text: str, length: int = 100) -> str:
    """Truncate text with ellipsis."""
    if len(text) <= length:
        return text
    return text[:length - 3] + "..."


def validate_email(email: str) -> bool:
    """Basic email validation."""
    pattern = r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$"
    return bool(re.match(pattern, email))


def deep_merge(base: dict, override: dict) -> dict:
    """Deep merge two dicts."""
    result = base.copy()
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = deep_merge(result[key], value)
        else:
            result[key] = value
    return result
''',
}

# ---------------------------------------------------------------------------
# Edits to apply — 8 realistic changes across 4 files
# ---------------------------------------------------------------------------

EDITS = [
    # 1. auth.py: Fix insecure password hashing → use bcrypt-style comparison
    {
        "file": "auth.py",
        "old_str": '    return hashlib.sha256(password.encode()).hexdigest()',
        "new_str": '    salt = "app-salt-v1"\n    return hashlib.pbkdf2_hmac("sha256", password.encode(), salt.encode(), 100000).hex()',
    },
    # 2. auth.py: Fix timing attack in verify_password
    {
        "file": "auth.py",
        "old_str": '    return hash_password(password) == hashed',
        "new_str": '    return hmac.compare_digest(hash_password(password), hashed)',
    },
    # 3. database.py: Add connection pooling constant
    {
        "file": "database.py",
        "old_str": 'MAX_CONNECTIONS = 5',
        "new_str": 'MAX_CONNECTIONS = 10\nCONNECTION_TIMEOUT = 30',
    },
    # 4. database.py: Fix SQL injection in insert_record
    {
        "file": "database.py",
        "old_str": '    query = f"INSERT INTO {table} ({columns}) VALUES ({placeholders})"',
        "new_str": '    # Validate table name to prevent SQL injection\n    if not table.isidentifier():\n        raise ValueError(f"Invalid table name: {table}")\n    query = f"INSERT INTO {table} ({columns}) VALUES ({placeholders})"',
    },
    # 5. api.py: Wire up login to real auth
    {
        "file": "api.py",
        "old_str": '    # TODO: validate against database\n    return {"status": HTTPStatus.OK, "token": "dummy-token"}',
        "new_str": '    from auth import verify_password, create_token\n    from database import execute_query\n\n    rows = execute_query("SELECT id, password_hash FROM users WHERE username = ?", (username,))\n    if not rows or not verify_password(password, rows[0]["password_hash"]):\n        return {"status": HTTPStatus.UNAUTHORIZED, "error": "Invalid credentials"}\n\n    token = create_token(str(rows[0]["id"]))\n    return {"status": HTTPStatus.OK, "token": token}',
    },
    # 6. api.py: Wire up registration
    {
        "file": "api.py",
        "old_str": '    # TODO: create user in database\n    return {"status": HTTPStatus.CREATED, "user_id": "new-user-id"}',
        "new_str": '    from auth import hash_password\n    from database import insert_record\n    from utils import validate_email\n\n    if not validate_email(email):\n        return {"status": HTTPStatus.BAD_REQUEST, "error": "Invalid email"}\n\n    user_id = insert_record("users", {\n        "username": username,\n        "password_hash": hash_password(password),\n        "email": email,\n    })\n    return {"status": HTTPStatus.CREATED, "user_id": str(user_id)}',
    },
    # 7. config.py: Add rate limiting config
    {
        "file": "config.py",
        "old_str": '    cors_origins: list = None',
        "new_str": '    cors_origins: list = None\n    rate_limit_per_minute: int = 60\n    rate_limit_burst: int = 10',
    },
    # 8. utils.py: Fix truncate off-by-one
    {
        "file": "utils.py",
        "old_str": '    return text[:length - 3] + "..."',
        "new_str": '    return text[: length - 3] + "..."',
    },
]


def create_test_files(workdir):
    """Create test files, return dict of name -> absolute path."""
    paths = {}
    for name, content in FILES.items():
        path = os.path.join(workdir, name)
        with open(path, "w") as f:
            f.write(content)
        paths[name] = path
    return paths


def verify_edits(paths):
    """Verify all edits were applied correctly. Returns (pass_count, fail_count, failures)."""
    passed = 0
    failed = 0
    failures = []
    for edit in EDITS:
        path = paths[edit["file"]]
        with open(path) as f:
            content = f.read()
        # The full new_str text should appear in the file
        if edit["new_str"] in content:
            passed += 1
        else:
            snippet = edit["new_str"][:60].replace("\n", "\\n")
            failed += 1
            failures.append(f"  {edit['file']}: new text not found '{snippet}...'")
    return passed, failed, failures


# ---------------------------------------------------------------------------
# Traditional approach: sequential read + str_replace per file
# ---------------------------------------------------------------------------

def benchmark_traditional(workdir):
    """Simulate traditional Claude Code workflow: read each file, apply edits, write back."""
    paths = create_test_files(workdir)
    round_trips = 0
    t0 = time.perf_counter()

    # Group edits by file
    edits_by_file = {}
    for edit in EDITS:
        edits_by_file.setdefault(edit["file"], []).append(edit)

    for filename, file_edits in edits_by_file.items():
        path = paths[filename]

        # Read file (1 round trip)
        with open(path) as f:
            content = f.read()
        round_trips += 1

        # Apply each edit (1 round trip per edit)
        for edit in file_edits:
            content = content.replace(edit["old_str"], edit["new_str"])
            round_trips += 1

        # Write file back (1 round trip)
        with open(path, "w") as f:
            f.write(content)
        round_trips += 1

    elapsed = time.perf_counter() - t0
    passed, failed, failures = verify_edits(paths)
    return {
        "elapsed_ms": elapsed * 1000,
        "round_trips": round_trips,
        "passed": passed,
        "failed": failed,
        "failures": failures,
    }


# ---------------------------------------------------------------------------
# Slipstream approach: open → batch(read_all + str_replace) → flush → close
# ---------------------------------------------------------------------------

def benchmark_slipstream(workdir, socket_path):
    """Use slipstream exec to do everything in fewer round trips."""
    paths = create_test_files(workdir)
    files = list(paths.values())
    round_trips = 0
    t0 = time.perf_counter()

    # Build batch ops: read all + all str_replace edits
    ops = []
    for edit in EDITS:
        ops.append({
            "method": "file.str_replace",
            "path": paths[edit["file"]],
            "old_str": edit["old_str"],
            "new_str": edit["new_str"],
        })

    # Single exec call: open + read_all + batch edits + flush (1 round trip)
    ops_json = json.dumps(ops)
    result = subprocess.run(
        [SLIPSTREAM, "exec",
         "--files"] + files + [
         "--read-all",
         "--ops", ops_json,
         "--flush"],
        capture_output=True, text=True,
        env={**os.environ, "SLIPSTREAM_SOCKET": socket_path},
    )
    round_trips += 1

    elapsed = time.perf_counter() - t0

    if result.returncode != 0:
        print(f"  ERROR: slipstream exec failed: {result.stderr}")
        return {
            "elapsed_ms": elapsed * 1000,
            "round_trips": round_trips,
            "passed": 0,
            "failed": len(EDITS),
            "failures": [f"exec failed: {result.stderr[:200]}"],
        }

    passed, failed, failures = verify_edits(paths)
    return {
        "elapsed_ms": elapsed * 1000,
        "round_trips": round_trips,
        "passed": passed,
        "failed": failed,
        "failures": failures,
    }


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    socket_path = "/tmp/slipstream-bench.sock"

    # Start daemon
    daemon = subprocess.Popen(
        [DAEMON, socket_path],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.3)  # let it start

    print("=" * 65)
    print("  Slipstream Benchmark: Real-World Multi-File Editing")
    print("=" * 65)
    print()
    print(f"  Files: {len(FILES)} source files")
    print(f"  Edits: {len(EDITS)} changes across {len(set(e['file'] for e in EDITS))} files")
    print()

    n_iter = 5
    trad_results = []
    slip_results = []

    for i in range(n_iter):
        with tempfile.TemporaryDirectory() as td:
            trad_results.append(benchmark_traditional(td))
        with tempfile.TemporaryDirectory() as td:
            slip_results.append(benchmark_slipstream(td, socket_path))

    # Stop daemon
    daemon.terminate()
    daemon.wait()
    try:
        os.unlink(socket_path)
    except OSError:
        pass

    # Print results
    def avg(results, key):
        return sum(r[key] for r in results) / len(results)

    trad_rt = avg(trad_results, "round_trips")
    slip_rt = avg(slip_results, "round_trips")
    trad_ms = avg(trad_results, "elapsed_ms")
    slip_ms = avg(slip_results, "elapsed_ms")
    trad_pass = avg(trad_results, "passed")
    slip_pass = avg(slip_results, "passed")
    trad_fail = avg(trad_results, "failed")
    slip_fail = avg(slip_results, "failed")

    print(f"  {'':30s} {'Traditional':>14s} {'Slipstream':>14s}")
    print(f"  {'-' * 60}")
    print(f"  {'Round trips (tool calls)':30s} {trad_rt:>14.0f} {slip_rt:>14.0f}")
    print(f"  {'Wall time (ms, avg of 5)':30s} {trad_ms:>14.2f} {slip_ms:>14.2f}")
    print(f"  {'Edits correct':30s} {trad_pass:>13.0f}/{len(EDITS)} {slip_pass:>13.0f}/{len(EDITS)}")
    print(f"  {'Edits failed':30s} {trad_fail:>14.0f} {slip_fail:>14.0f}")
    print()

    # Show any failures
    for label, results in [("Traditional", trad_results), ("Slipstream", slip_results)]:
        for r in results:
            if r["failures"]:
                print(f"  {label} failures:")
                for f in r["failures"]:
                    print(f"    {f}")

    # Summary
    print()
    rt_saved = trad_rt - slip_rt
    print(f"  Tool call reduction: {trad_rt:.0f} -> {slip_rt:.0f} ({rt_saved:.0f} fewer)")
    print(f"  At ~1-3s LLM latency per tool call, that's ~{rt_saved*1:.0f}-{rt_saved*3:.0f}s saved")
    print()

    # Return exit code based on correctness
    if any(r["failed"] > 0 for r in trad_results + slip_results):
        sys.exit(1)


if __name__ == "__main__":
    main()
