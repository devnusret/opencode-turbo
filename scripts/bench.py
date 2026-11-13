#!/usr/bin/env python3
"""Measure first output and first full-frame time of a terminal application.

This starts the command in a PTY, records when the first byte arrives, and
records when at least --threshold bytes have arrived (a rough proxy for the
first fully painted screen). The process group is killed at the end of each run.

Example:
    python3 scripts/bench.py --runs 3 -- opencode
    python3 scripts/bench.py --runs 2 --timeout 15 -- opencode-turbo --turbo-no-fast-boot
"""

from __future__ import annotations

import argparse
import json
import os
import pty
import select
import signal
import statistics
import sys
import time


def run_once(command: list[str], timeout: float, threshold: int) -> dict:
    pid, fd = pty.fork()
    if pid == 0:
        environment = dict(os.environ)
        environment.setdefault("TERM", "xterm-256color")
        try:
            os.execvpe(command[0], command, environment)
        except OSError as error:
            os.write(2, f"exec failed: {error}\n".encode())
            os._exit(127)

    started = time.monotonic()
    first_ms = None
    frame_ms = None
    total = bytearray()

    try:
        while time.monotonic() - started < timeout:
            readable, _, _ = select.select([fd], [], [], 0.05)
            if not readable:
                continue
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            now_ms = (time.monotonic() - started) * 1000.0
            if first_ms is None:
                first_ms = now_ms
            total.extend(chunk)
            if frame_ms is None and len(total) >= threshold:
                frame_ms = now_ms
    finally:
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
        try:
            os.close(fd)
        except OSError:
            pass

    return {
        "first_ms": round(first_ms, 1) if first_ms is not None else None,
        "frame_ms": round(frame_ms, 1) if frame_ms is not None else None,
        "bytes": len(total),
    }


def median(values: list[float]) -> float | None:
    present = [value for value in values if value is not None]
    if not present:
        return None
    return round(statistics.median(present), 1)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=3, help="runs per command (default: 3)")
    parser.add_argument("--timeout", type=float, default=12.0, help="seconds per run (default: 12)")
    parser.add_argument(
        "--threshold",
        type=int,
        default=6000,
        help="bytes that count as the first full frame (default: 6000)",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER, help="command after --")
    args = parser.parse_args()

    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        parser.error("provide a command after --")

    results = []
    for index in range(args.runs):
        result = run_once(command, args.timeout, args.threshold)
        results.append(result)
        print(f"run {index + 1}: {json.dumps(result)}", file=sys.stderr, flush=True)

    summary = {
        "command": command,
        "runs": args.runs,
        "median_first_ms": median([result["first_ms"] for result in results]),
        "median_frame_ms": median([result["frame_ms"] for result in results]),
        "results": results,
    }
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
