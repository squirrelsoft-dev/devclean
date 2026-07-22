#!/usr/bin/env python3
"""Run `qlty check` from the repository root for agent stop hooks."""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
from pathlib import Path


HOOK_ENV = "OFFCUT_QLTY_STOP_HOOK_ACTIVE"
TIMEOUT_ENV = "OFFCUT_QLTY_CHECK_TIMEOUT_SECONDS"
MAX_REASON_CHARS = 6000
DEFAULT_TIMEOUT_SECONDS = 540.0
GROUP_KILL_GRACE_SECONDS = 5.0
FINDINGS_EXIT_CODE = 1
QLTY_CONFIG_RELATIVE = Path(".qlty") / "qlty.toml"
INVOCATION_RESULT_KEY = "exitResult"
INVOCATION_ERROR_SUFFIX = "_ERROR"


class RootResolutionError(RuntimeError):
    """The repository root could not be determined from inside the repository."""


class ToolUnavailableError(RuntimeError):
    """A required executable is missing from the environment the agent runs in."""


class CheckTimeoutError(RuntimeError):
    """`qlty check` outlived the wrapper budget and its process group was killed."""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run qlty check for agent stop hooks")
    parser.add_argument(
        "--tool", choices=["codex", "claude", "pi", "generic"], default="generic"
    )
    parser.add_argument(
        "--cwd", help="Fallback working directory when hook input omits cwd"
    )
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


def allow_stop() -> int:
    return 0


def block_stop(tool: str, reason: str) -> int:
    if is_json_stop_tool(tool):
        output_json({"decision": "block", "reason": reason})
        return 0

    print(reason, file=sys.stderr)
    return 1


def skip_check(detail: str, remedy: str) -> int:
    print(
        f"qlty check was skipped: {detail}\n\n"
        f"{remedy}\n\n"
        "This stop was not blocked, and no repository file needs to change.",
        file=sys.stderr,
    )
    return 1


def qlty_config_path(root: Path) -> Path:
    return root / QLTY_CONFIG_RELATIVE


def hook_project_root() -> Path:
    return Path(__file__).resolve().parents[2]


def same_directory(left: Path, right: Path) -> bool:
    try:
        return left.samefile(right)
    except OSError:
        return left == right


def ancestors_within_project(path: Path) -> list[Path]:
    boundary = hook_project_root()
    chain: list[Path] = []
    for candidate in [path, *path.parents]:
        chain.append(candidate)
        if same_directory(candidate, boundary):
            return chain
    return []


def find_repo_root(cwd: Path) -> Path:
    git_root: Path | None = None
    git_unavailable = False
    try:
        git = subprocess.run(
            ["git", "-C", str(cwd), "rev-parse", "--show-toplevel"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        git_unavailable = True
        detail = f"git could not be executed ({error})"
    else:
        if git.returncode == 0:
            git_root = Path(git.stdout.strip()).resolve()
            if not ancestors_within_project(git_root):
                detail = (
                    f"{git_root} is outside {hook_project_root()}, "
                    "the project this hook is installed in"
                )
            elif qlty_config_path(git_root).is_file():
                return git_root
            else:
                detail = f"{git_root} is a git root without {QLTY_CONFIG_RELATIVE}"
        else:
            detail = git.stderr.strip() or "not a git repository"

    for candidate in ancestors_within_project(cwd.resolve()):
        if qlty_config_path(candidate).is_file():
            return candidate

    if git_root is not None and not qlty_config_path(git_root).is_file():
        return git_root

    message = f"could not resolve repository root from {cwd}: {detail}"
    if git_unavailable:
        raise ToolUnavailableError(message)
    raise RootResolutionError(message)


def timeout_seconds() -> float:
    raw = os.environ.get(TIMEOUT_ENV, "").strip()
    if not raw:
        return DEFAULT_TIMEOUT_SECONDS
    try:
        value = float(raw)
    except ValueError:
        return DEFAULT_TIMEOUT_SECONDS
    return value if value > 0 else DEFAULT_TIMEOUT_SECONDS


def terminate_process_group(process: subprocess.Popen[str]) -> None:
    escalation = [signal.SIGTERM, getattr(signal, "SIGKILL", signal.SIGTERM)]
    for sig in escalation:
        try:
            os.killpg(os.getpgid(process.pid), sig)
        except (AttributeError, OSError):
            process.kill()
        try:
            process.communicate(timeout=GROUP_KILL_GRACE_SECONDS)
            return
        except subprocess.TimeoutExpired:
            continue


def run_check(root: Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    budget = timeout_seconds()
    process = subprocess.Popen(
        ["qlty", "check", "--no-progress", "--no-upgrade-check", "--print-errors"],
        cwd=root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=budget)
    except subprocess.TimeoutExpired:
        terminate_process_group(process)
        raise CheckTimeoutError(
            f"qlty check timed out after {budget:g}s in {root}"
        ) from None
    return subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)


def linter_error_results(result: subprocess.CompletedProcess[str]) -> list[str]:
    errors = []
    for stream in [result.stdout, result.stderr]:
        for line in (stream or "").splitlines():
            key, separator, value = line.partition(":")
            if separator and key.strip() == INVOCATION_RESULT_KEY:
                outcome = value.strip()
                if outcome.endswith(INVOCATION_ERROR_SUFFIX):
                    errors.append(outcome)
    return errors


def captured_output(result: subprocess.CompletedProcess[str]) -> str:
    combined = "\n".join(
        part.strip() for part in [result.stdout, result.stderr] if part.strip()
    )
    if len(combined) > MAX_REASON_CHARS:
        combined = combined[:MAX_REASON_CHARS] + "\n... output truncated ..."
    return combined


def build_reason(root: Path, result: subprocess.CompletedProcess[str]) -> str:
    combined = captured_output(result)
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
        return allow_stop()

    if hook_input.get("stop_hook_active") is True:
        return allow_stop()

    cwd_value = (
        hook_input.get("cwd")
        or args.cwd
        or os.environ.get("CLAUDE_PROJECT_DIR")
        or os.getcwd()
    )
    cwd = Path(str(cwd_value))

    try:
        root = find_repo_root(cwd)
    except ToolUnavailableError as error:
        return skip_check(
            str(error),
            "Install git and make sure it is on PATH for the environment this agent runs in.",
        )
    except RootResolutionError as error:
        return block_stop(args.tool, str(error))

    config_path = qlty_config_path(root)
    if not config_path.is_file():
        return block_stop(
            args.tool,
            f"qlty stop hook is misconfigured: {config_path} does not exist.\n\n"
            "Restore the Qlty config for this repository (for example with `qlty init`), "
            "then stop again.",
        )

    env = os.environ.copy()
    env[HOOK_ENV] = "1"
    try:
        result = run_check(root, env)
    except OSError as error:
        return skip_check(
            f"qlty could not be executed in {root} ({error}).",
            "Install Qlty (https://qlty.sh) and make sure `qlty` is on PATH for the "
            "environment this agent runs in — the installer puts it in ~/.qlty/bin and "
            "only adds that to PATH via your shell profile, so agents launched outside a "
            "login shell may not see it.",
        )
    except CheckTimeoutError as error:
        return skip_check(
            f"{error}.",
            "The whole qlty process group was killed, so no linter is still running in the "
            "background. Run `qlty check --no-progress --no-upgrade-check --print-errors` "
            "by hand to see how long a full run takes here (a cold plugin cache is the "
            f"usual cause), or raise the budget with {TIMEOUT_ENV}.",
        )

    if result.returncode == 0:
        return allow_stop()

    linter_errors = linter_error_results(result)
    if result.returncode == FINDINGS_EXIT_CODE and not linter_errors:
        return block_stop(args.tool, build_reason(root, result))

    summary = (
        f"qlty reported linter errors ({len(linter_errors)} failed invocation(s))"
        if linter_errors
        else "qlty check could not complete"
    )
    detail = captured_output(result)
    return skip_check(
        f"{summary} in {root} (exit code {result.returncode})."
        + (f"\n\n{detail}" if detail else ""),
        f"qlty exits {FINDINGS_EXIT_CODE} for findings an agent can act on. Any other exit "
        f"code, and any invocation whose {INVOCATION_RESULT_KEY} ends in "
        f"{INVOCATION_ERROR_SUFFIX}, means a linter could not run at all — a failed plugin "
        "install, a cold-cache network failure, or an unsupported runtime. Repair the Qlty "
        "setup with `qlty check --no-progress --no-upgrade-check --print-errors` by hand.",
    )


if __name__ == "__main__":
    raise SystemExit(main())
