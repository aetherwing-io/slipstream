#!/usr/bin/env python3
"""
Benchmark: Slipstream MCP vs traditional file I/O for multi-file editing.

Simulates a realistic LLM agent workflow:
  - Open 3 files
  - Read specific regions from each
  - Make edits to 2 of them
  - Flush changes to disk
  - Close the session

Measures wall-clock time and counts "round trips" (tool calls).
"""

import json
import os
import subprocess
import tempfile
import time


def create_test_files(tmpdir, n_files=3, lines_per_file=100):
    """Create test files with numbered lines."""
    files = []
    for i in range(n_files):
        path = os.path.join(tmpdir, f"file_{i}.txt")
        with open(path, "w") as f:
            for line_num in range(lines_per_file):
                f.write(f"file{i}_line{line_num}: {'x' * 40}\n")
        files.append(path)
    return files


def benchmark_traditional(files):
    """
    Simulate traditional file editing: each operation = separate shell command.
    This is what Claude Code does today with Read/Edit tools.
    """
    round_trips = 0
    t0 = time.perf_counter()

    # "Read" each file (3 round trips)
    contents = {}
    for f in files:
        with open(f) as fh:
            contents[f] = fh.readlines()
        round_trips += 1

    # "Read range" from 2 files (2 round trips)
    _ = contents[files[0]][10:20]
    round_trips += 1
    _ = contents[files[1]][50:60]
    round_trips += 1

    # "Edit" file 0, lines 10-15 (1 round trip)
    new_lines = [f"EDITED_LINE_{i}\n" for i in range(5)]
    contents[files[0]][10:15] = new_lines
    round_trips += 1

    # "Edit" file 1, lines 50-55 (1 round trip)
    new_lines = [f"MODIFIED_{i}\n" for i in range(5)]
    contents[files[1]][50:55] = new_lines
    round_trips += 1

    # "Write" both files to disk (2 round trips)
    for f in [files[0], files[1]]:
        with open(f, "w") as fh:
            fh.writelines(contents[f])
        round_trips += 1

    elapsed = time.perf_counter() - t0
    return elapsed, round_trips


class McpClient:
    """MCP client for benchmarking."""

    def __init__(self, socket_path):
        env = os.environ.copy()
        env["SLIPSTREAM_SOCKET"] = socket_path
        self.proc = subprocess.Popen(
            [
                os.path.join(
                    os.path.dirname(__file__),
                    "..",
                    "target",
                    "release",
                    "slipstream-mcp",
                )
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        self.next_id = 1
        self._initialize()

    def _send(self, obj):
        line = json.dumps(obj) + "\n"
        self.proc.stdin.write(line.encode())
        self.proc.stdin.flush()

    def _recv(self):
        line = self.proc.stdout.readline()
        return json.loads(line)

    def _initialize(self):
        self._send(
            {
                "jsonrpc": "2.0",
                "id": self.next_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "benchmark", "version": "0.1.0"},
                },
            }
        )
        self.next_id += 1
        self._recv()
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        time.sleep(0.05)

    def call_tool(self, name, arguments):
        self._send(
            {
                "jsonrpc": "2.0",
                "id": self.next_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        )
        self.next_id += 1
        r = self._recv()
        text = r["result"]["content"][0]["text"]
        return json.loads(text)

    def close(self):
        self.proc.terminate()
        self.proc.wait()


def benchmark_slipstream(files, socket_path):
    """
    Same editing task using slipstream MCP tools.
    Key: batch operations reduce round trips.
    """
    client = McpClient(socket_path)
    round_trips = 0
    t0 = time.perf_counter()

    # Open all 3 files in one call (1 round trip)
    result = client.call_tool("slipstream_open", {"files": files})
    sid = result["session_id"]
    round_trips += 1

    # Batch: read ranges from 2 files + write to 2 files (1 round trip!)
    result = client.call_tool(
        "slipstream_batch",
        {
            "session_id": sid,
            "ops": [
                {
                    "method": "file.read",
                    "path": files[0],
                    "start": 10,
                    "end": 20,
                },
                {
                    "method": "file.read",
                    "path": files[1],
                    "start": 50,
                    "end": 60,
                },
                {
                    "method": "file.write",
                    "path": files[0],
                    "start": 10,
                    "end": 15,
                    "content": [f"EDITED_LINE_{i}" for i in range(5)],
                },
                {
                    "method": "file.write",
                    "path": files[1],
                    "start": 50,
                    "end": 55,
                    "content": [f"MODIFIED_{i}" for i in range(5)],
                },
            ],
        },
    )
    round_trips += 1

    # Flush all edits to disk (1 round trip)
    client.call_tool("slipstream_flush", {"session_id": sid})
    round_trips += 1

    elapsed = time.perf_counter() - t0

    # Close session (cleanup, not counted in the edit workflow)
    client.call_tool("slipstream_close", {"session_id": sid})

    client.close()
    return elapsed, round_trips


def main():
    socket_path = "/tmp/slipstream.sock"

    # Verify daemon is running
    if not os.path.exists(socket_path):
        print("ERROR: Start the daemon first: slipstream-daemon /tmp/slipstream.sock &")
        return

    print("=" * 60)
    print("Slipstream Benchmark: MCP vs Traditional File I/O")
    print("=" * 60)
    print()

    # Run 5 iterations
    n_iter = 5
    trad_times, trad_rts = [], []
    slip_times, slip_rts = [], []

    for i in range(n_iter):
        with tempfile.TemporaryDirectory() as tmpdir:
            files = create_test_files(tmpdir)

            # Traditional
            t, rt = benchmark_traditional(files)
            trad_times.append(t)
            trad_rts.append(rt)

        with tempfile.TemporaryDirectory() as tmpdir:
            files = create_test_files(tmpdir)

            # Slipstream
            t, rt = benchmark_slipstream(files, socket_path)
            slip_times.append(t)
            slip_rts.append(rt)

    print(f"Task: Open 3 files, read 2 ranges, edit 2 files, flush to disk")
    print(f"Iterations: {n_iter}")
    print()
    print(f"{'':30s} {'Traditional':>15s} {'Slipstream MCP':>15s} {'Reduction':>12s}")
    print("-" * 75)

    avg_trad_rt = sum(trad_rts) / len(trad_rts)
    avg_slip_rt = sum(slip_rts) / len(slip_rts)
    rt_reduction = (1 - avg_slip_rt / avg_trad_rt) * 100
    print(f"{'Round trips (tool calls)':30s} {avg_trad_rt:>15.0f} {avg_slip_rt:>15.0f} {rt_reduction:>11.0f}%")

    avg_trad_t = sum(trad_times) / len(trad_times)
    avg_slip_t = sum(slip_times) / len(slip_times)
    print(f"{'Avg wall time (ms)':30s} {avg_trad_t*1000:>15.2f} {avg_slip_t*1000:>15.2f}")
    print()

    print("KEY INSIGHT: Round trips matter because each one is an LLM ↔ tool")
    print(f"boundary crossing. Slipstream reduces {avg_trad_rt:.0f} → {avg_slip_rt:.0f} tool calls")
    print(f"({rt_reduction:.0f}% fewer). In real usage, each round trip adds ~200-500ms of")
    print("LLM inference latency, so the actual time savings would be:")
    print()
    saved = avg_trad_rt - avg_slip_rt
    print(f"  {saved:.0f} fewer round trips × ~300ms each = ~{saved * 0.3:.1f}s saved per editing task")
    print()

    # Verify correctness
    with tempfile.TemporaryDirectory() as tmpdir:
        files = create_test_files(tmpdir)
        benchmark_slipstream(files, socket_path)
        with open(files[0]) as f:
            lines = f.readlines()
        assert lines[10].startswith("EDITED_LINE_0"), f"Unexpected: {lines[10]}"
        assert lines[14].startswith("EDITED_LINE_4"), f"Unexpected: {lines[14]}"
        with open(files[1]) as f:
            lines = f.readlines()
        assert lines[50].startswith("MODIFIED_0"), f"Unexpected: {lines[50]}"
        print("Correctness verified: edits applied correctly to disk.")


if __name__ == "__main__":
    main()
