#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "${repo_root}"

python3 - <<'PY'
import json
import tomllib
from pathlib import Path

root = Path.cwd()

codex_project_config = root / ".codex" / "config.toml"
if codex_project_config.exists():
    # Codex discards project-local `[features]`, and `hooks` is a stable feature
    # that already defaults to enabled, so committing one is a misleading no-op.
    assert "features" not in tomllib.loads(codex_project_config.read_text())

codex_hooks = json.loads((root / ".codex" / "hooks.json").read_text())
assert set(codex_hooks) == {"hooks"}
codex_stop = codex_hooks["hooks"]["Stop"]
assert len(codex_stop) == 1
codex_handler = codex_stop[0]["hooks"][0]
assert codex_handler["type"] == "command"
assert ".qlty/hooks/qlty-check.py" in codex_handler["command"]
assert "--tool codex" in codex_handler["command"]
assert codex_handler["timeout"] == 600

claude_settings = json.loads((root / ".claude" / "settings.json").read_text())
assert "disableAllHooks" not in claude_settings
claude_stop = claude_settings["hooks"]["Stop"]
assert len(claude_stop) == 1
assert claude_stop[0]["matcher"] == ""
claude_handler = claude_stop[0]["hooks"][0]
assert claude_handler["type"] == "command"
assert claude_handler["command"] == "python3"
assert claude_handler["args"] == [
    "${CLAUDE_PROJECT_DIR}/.qlty/hooks/qlty-check.py",
    "--tool",
    "claude",
]
assert claude_handler["timeout"] == 600

pi_extension = (root / ".pi" / "extensions" / "qlty-stop-hook.ts").read_text()
assert 'pi.on("session_shutdown"' in pi_extension
assert ".qlty" in pi_extension
assert "qlty-check.py" in pi_extension
assert "OFFCUT_QLTY_STOP_HOOK_ACTIVE" in pi_extension
assert 'event.reason !== "quit"' in pi_extension
assert "console.error(message)" in pi_extension
assert pi_extension.index("Qlty stop hook: running qlty check") < pi_extension.index('pi.exec("python3"')
PY

python3 -m py_compile "${repo_root}/.qlty/hooks/qlty-check.py"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

mkdir -p "${tmp}/repo/.qlty" "${tmp}/repo/subdir" "${tmp}/bin"
git -C "${tmp}/repo" init --quiet
touch "${tmp}/repo/.qlty/qlty.toml"
repo_real="$(cd "${tmp}/repo" && pwd -P)"

cat > "${tmp}/bin/qlty" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s\nargs=%s\n' "$PWD" "$*" > "${QLTY_STUB_LOG:?}"
printf 'stub qlty passed\n'
exit 0
SH
chmod +x "${tmp}/bin/qlty"

export PATH="${tmp}/bin:${PATH}"
export QLTY_STUB_LOG="${tmp}/qlty-success.log"

printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo/subdir" |
  python3 "${repo_root}/.qlty/hooks/qlty-check.py" --tool codex > "${tmp}/success.out"

test ! -s "${tmp}/success.out"
grep -F "cwd=${repo_real}" "${QLTY_STUB_LOG}" >/dev/null
grep -F "args=check --no-progress --no-upgrade-check" "${QLTY_STUB_LOG}" >/dev/null

cat > "${tmp}/bin/qlty" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s\nargs=%s\n' "$PWD" "$*" > "${QLTY_STUB_LOG:?}"
echo "problem from stdout"
echo "problem from stderr" >&2
exit 7
SH
chmod +x "${tmp}/bin/qlty"

export QLTY_STUB_LOG="${tmp}/qlty-failure.log"

printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo" |
  python3 "${repo_root}/.qlty/hooks/qlty-check.py" --tool claude > "${tmp}/failure.json"

python3 - <<'PY' "${tmp}/failure.json"
import json
import sys
payload = json.loads(open(sys.argv[1]).read())
assert payload["decision"] == "block"
assert "qlty check failed" in payload["reason"]
assert "problem from stdout" in payload["reason"]
assert "problem from stderr" in payload["reason"]
PY

grep -F "cwd=${repo_real}" "${QLTY_STUB_LOG}" >/dev/null

export QLTY_STUB_LOG="${tmp}/qlty-guard.log"
rm -f "${QLTY_STUB_LOG}"
printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":true}\n' "${tmp}/repo" |
  python3 "${repo_root}/.qlty/hooks/qlty-check.py" --tool codex > "${tmp}/guard.out"

test ! -e "${QLTY_STUB_LOG}"
test ! -s "${tmp}/guard.out"

mkdir -p "${tmp}/noconfig"
git -C "${tmp}/noconfig" init --quiet

status=0
printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/noconfig" |
  python3 "${repo_root}/.qlty/hooks/qlty-check.py" --tool pi \
    > "${tmp}/misconfig.out" 2> "${tmp}/misconfig.err" || status=$?

test "${status}" -eq 1
test ! -s "${tmp}/misconfig.out"
grep -F "qlty stop hook is misconfigured" "${tmp}/misconfig.err" >/dev/null
grep -F "qlty init" "${tmp}/misconfig.err" >/dev/null

python3_bin="$(command -v python3)"
mkdir -p "${tmp}/nopath"
ln -s "$(command -v git)" "${tmp}/nopath/git"

printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo" |
  env PATH="${tmp}/nopath" "${python3_bin}" "${repo_root}/.qlty/hooks/qlty-check.py" \
    --tool claude > "${tmp}/missing.json" 2> "${tmp}/missing.err"

test ! -s "${tmp}/missing.err"
python3 - <<'PY' "${tmp}/missing.json"
import json
import sys
payload = json.loads(open(sys.argv[1]).read())
assert payload["decision"] == "block"
assert "qlty could not be executed" in payload["reason"]
assert "PATH" in payload["reason"]
PY

status=0
printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo" |
  env PATH="${tmp}/empty-path" "${python3_bin}" "${repo_root}/.qlty/hooks/qlty-check.py" \
    --tool pi > "${tmp}/nogit.out" 2> "${tmp}/nogit.err" || status=$?

test "${status}" -eq 1
test ! -s "${tmp}/nogit.out"
grep -F "qlty could not be executed" "${tmp}/nogit.err" >/dev/null

echo "agent hook config tests passed"
