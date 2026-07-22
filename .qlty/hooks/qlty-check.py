#!/usr/bin/env python3
"""Run `qlty check` from the repository root for agent stop hooks."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path


HOOK_ENV = "OFFCUT_QLTY_STOP_HOOK_ACTIVE"
MAX_REASON_CHARS = 6000


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run qlty check for agent stop hooks")
    parser.add_argument("--tool", choices=["codex", "claude", "pi", "generic"], default="generic")
    parser.add_argument("--cwd", help="Fallback working directory when hook input omits cwd")
    parser.add_argument("--event", help="Lifecycle event name for tools that do not pass JSON input")
    return parser.parse_args()


def read_hook_input() -> dict[str, object]:
    data = sys.stdin.read()
    if not data.strip():
        return {}
    try:
        parsed = json.loads(data)
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def output_json(payload: dict[str, object]) -> None:
    json.dump(payload, sys.stdout, separators=(",", ":"))
    sys.stdout.write("\n")


def is_json_stop_tool(tool: str) -> bool:
    return tool in {"codex", "claude"}


def allow_stop(tool: str) -> int:
    if is_json_stop_tool(tool):
        return 0
    return 0


def block_stop(tool: str, reason: str) -> int:
    if is_json_stop_tool(tool):
        output_json({"decision": "block", "reason": reason})
        return 0

    print(reason, file=sys.stderr)
    return 1


def find_repo_root(cwd: Path) -> Path:
    git = subprocess.run(
        ["git", "-C", str(cwd), "rev-parse", "--show-toplevel"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if git.returncode == 0:
        return Path(git.stdout.strip()).resolve()

    current = cwd.resolve()
    for candidate in [current, *current.parents]:
        if (candidate / ".qlty" / "qlty.toml").is_file():
            return candidate

    raise RuntimeError(
        f"could not resolve repository root from {cwd}: {git.stderr.strip() or 'not a git repository'}"
    )


def build_reason(root: Path, result: subprocess.CompletedProcess[str]) -> str:
    combined = "\n".join(part.strip() for part in [result.stdout, result.stderr] if part.strip())
    if len(combined) > MAX_REASON_CHARS:
        combined = combined[:MAX_REASON_CHARS] + "\n... output truncated ..."
    if not combined:
        combined = f"qlty check exited with code {result.returncode}"
    return (
        f"qlty check failed in {root} with exit code {result.returncode}.\n\n"
        f"{combined}\n\n"
        "Fix the reported Qlty issues, then stop again. "
        "The follow-up stop is allowed when the agent reports stop_hook_active=true."
    )


def main() -> int:
    args = parse_args()
    hook_input = read_hook_input()

    if os.environ.get(HOOK_ENV):
        return allow_stop(args.tool)

    if hook_input.get("stop_hook_active") is True:
        return allow_stop(args.tool)

    cwd_value = hook_input.get("cwd") or args.cwd or os.environ.get("CLAUDE_PROJECT_DIR") or os.getcwd()
    cwd = Path(str(cwd_value))

    try:
        root = find_repo_root(cwd)
    except RuntimeError as error:
        return block_stop(args.tool, str(error))

    config_path = root / ".qlty" / "qlty.toml"
    if not config_path.is_file():
        return block_stop(args.tool, f"qlty check skipped: {config_path} does not exist")

    env = os.environ.copy()
    env[HOOK_ENV] = "1"
    result = subprocess.run(
        ["qlty", "check", "--no-progress", "--no-upgrade-check"],
        cwd=root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    if result.returncode == 0:
        return allow_stop(args.tool)

    return block_stop(args.tool, build_reason(root, result))


if __name__ == "__main__":
    raise SystemExit(main())
