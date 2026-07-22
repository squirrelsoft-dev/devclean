#!/usr/bin/env bash
set -euo pipefail

# This script lives at <root>/tests, so its own location names the root exactly.
# Asking git would fail in a source tarball with no .git, and would name the outer
# root when Offcut is vendored as a plain subdirectory of another repository.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${repo_root}"

refute_match() {
  if grep -qi -- "$1" "$2"; then
    echo "assertion failed: '$2' must not match '$1'" >&2
    exit 1
  fi
}

python3 - <<'PY'
import json
import tomllib
from pathlib import Path

root = Path.cwd()

qlty_config = tomllib.loads((root / ".qlty" / "qlty.toml").read_text())
plugin_names = {plugin["name"] for plugin in qlty_config["plugin"]}
# The hook implementation this config gates is Python, TypeScript, and Bash. Without
# these plugins the quality gate would report clean on its own sources.
assert {"ruff", "biome", "shellcheck"} <= plugin_names, plugin_names

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
# An unguarded $(git rev-parse --show-toplevel) collapses to "" when git is missing
# or the cwd is not a worktree, so python3 would fail on "/.qlty/..." before the
# wrapper could apply its skip-vs-block taxonomy.
assert "$(git rev-parse --show-toplevel 2>/dev/null || pwd)" in codex_handler["command"]

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
assert '"python3"' in pi_extension
assert pi_extension.index("Qlty stop hook: running qlty check") < pi_extension.index("pi.exec(")
# The extension file lives at <root>/.pi/extensions, so its own location names the
# root exactly; shelling out to git would reintroduce a nested-repo failure mode.
assert 'resolve(import.meta.dirname, "..", "..")' in pi_extension
assert '"git"' not in pi_extension
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
grep -F "args=check --no-progress --no-upgrade-check --print-errors" "${QLTY_STUB_LOG}" >/dev/null

mkdir -p "${tmp}/repo/nested"
git -C "${tmp}/repo/nested" init --quiet
export QLTY_STUB_LOG="${tmp}/qlty-nested.log"

printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo/nested" |
  python3 "${repo_root}/.qlty/hooks/qlty-check.py" --tool codex > "${tmp}/nested.out"

test ! -s "${tmp}/nested.out"
# A nested git repository owning no .qlty/qlty.toml must not become the root: git
# names it, but the wrapper falls through to the nearest ancestor that owns a config
# instead of blocking the stop over a config the nested repo was never meant to have.
grep -F "cwd=${repo_real}" "${QLTY_STUB_LOG}" >/dev/null

cat > "${tmp}/bin/qlty" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s\nargs=%s\n' "$PWD" "$*" > "${QLTY_STUB_LOG:?}"
echo "problem from stdout"
echo "problem from stderr" >&2
exit 1
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

# Qlty exits 1 when it has findings to report and a different code (99 for this build)
# when it could not produce a report at all — a failed plugin install, a cold plugin
# cache with no network, an unsupported runtime. Those are environment failures, so
# they must fail open visibly rather than order the agent to fix findings that do not
# exist.
cat > "${tmp}/bin/qlty" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s\nargs=%s\n' "$PWD" "$*" > "${QLTY_STUB_LOG:?}"
echo "Error installing actionlint@1.7.9: status code 404" >&2
exit 99
SH
chmod +x "${tmp}/bin/qlty"

for tool in claude codex pi; do
  export QLTY_STUB_LOG="${tmp}/qlty-setup-${tool}.log"
  status=0
  printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo" |
    python3 "${repo_root}/.qlty/hooks/qlty-check.py" --tool "${tool}" \
      > "${tmp}/setup-${tool}.out" 2> "${tmp}/setup-${tool}.err" || status=$?

  test "${status}" -eq 1
  test ! -s "${tmp}/setup-${tool}.out"
  grep -F "qlty check was skipped" "${tmp}/setup-${tool}.err" >/dev/null
  grep -F "exit code 99" "${tmp}/setup-${tool}.err" >/dev/null
  grep -F "Error installing actionlint" "${tmp}/setup-${tool}.err" >/dev/null
  grep -F "no repository file needs to change" "${tmp}/setup-${tool}.err" >/dev/null
  refute_match "decision" "${tmp}/setup-${tool}.err"
  refute_match "Fix the reported Qlty issues" "${tmp}/setup-${tool}.err"
done

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
mkdir -p "${tmp}/nopath" "${tmp}/empty-path"
ln -s "$(command -v git)" "${tmp}/nopath/git"

for tool in claude codex pi; do
  status=0
  printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo" |
    env PATH="${tmp}/nopath" "${python3_bin}" "${repo_root}/.qlty/hooks/qlty-check.py" \
      --tool "${tool}" > "${tmp}/missing-${tool}.out" 2> "${tmp}/missing-${tool}.err" || status=$?

  test "${status}" -eq 1
  test ! -s "${tmp}/missing-${tool}.out"
  grep -F "qlty check was skipped" "${tmp}/missing-${tool}.err" >/dev/null
  grep -F "PATH" "${tmp}/missing-${tool}.err" >/dev/null
  grep -F "no repository file needs to change" "${tmp}/missing-${tool}.err" >/dev/null
  refute_match "remove the project stop hook" "${tmp}/missing-${tool}.err"
  refute_match "decision" "${tmp}/missing-${tool}.err"
done

refute_match "remove the project stop hook" "${repo_root}/.qlty/hooks/qlty-check.py"

status=0
printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo" |
  env PATH="${tmp}/empty-path" "${python3_bin}" "${repo_root}/.qlty/hooks/qlty-check.py" \
    --tool pi > "${tmp}/nogit.out" 2> "${tmp}/nogit.err" || status=$?

test "${status}" -eq 1
test ! -s "${tmp}/nogit.out"
grep -F "qlty check was skipped" "${tmp}/nogit.err" >/dev/null
grep -F "qlty could not be executed" "${tmp}/nogit.err" >/dev/null

for tool in claude codex pi; do
  status=0
  printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/noconfig" |
    env PATH="${tmp}/empty-path" "${python3_bin}" "${repo_root}/.qlty/hooks/qlty-check.py" \
      --tool "${tool}" > "${tmp}/nogitroot-${tool}.out" 2> "${tmp}/nogitroot-${tool}.err" || status=$?

  test "${status}" -eq 1
  test ! -s "${tmp}/nogitroot-${tool}.out"
  grep -F "qlty check was skipped" "${tmp}/nogitroot-${tool}.err" >/dev/null
  grep -F "git could not be executed" "${tmp}/nogitroot-${tool}.err" >/dev/null
  grep -F "no repository file needs to change" "${tmp}/nogitroot-${tool}.err" >/dev/null
  refute_match "decision" "${tmp}/nogitroot-${tool}.err"
  refute_match "remove the project stop hook" "${tmp}/nogitroot-${tool}.err"
done

cat > "${tmp}/bin/qlty" <<'SH'
#!/usr/bin/env bash
( sleep 3; touch "${QLTY_STUB_GRANDCHILD_MARKER:?}" ) &
sleep 30
SH
chmod +x "${tmp}/bin/qlty"

export QLTY_STUB_GRANDCHILD_MARKER="${tmp}/grandchild-survived"

status=0
printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/repo" |
  env OFFCUT_QLTY_CHECK_TIMEOUT_SECONDS=1 \
    "${python3_bin}" "${repo_root}/.qlty/hooks/qlty-check.py" --tool claude \
      > "${tmp}/timeout.out" 2> "${tmp}/timeout.err" || status=$?

test "${status}" -eq 1
test ! -s "${tmp}/timeout.out"
grep -F "qlty check was skipped" "${tmp}/timeout.err" >/dev/null
grep -F "qlty check timed out" "${tmp}/timeout.err" >/dev/null
grep -F "OFFCUT_QLTY_CHECK_TIMEOUT_SECONDS" "${tmp}/timeout.err" >/dev/null
refute_match "decision" "${tmp}/timeout.err"

sleep 4
test ! -e "${QLTY_STUB_GRANDCHILD_MARKER}"

mkdir -p "${tmp}/plain"

printf '{"cwd":"%s","hook_event_name":"Stop","stop_hook_active":false}\n' "${tmp}/plain" |
  python3 "${repo_root}/.qlty/hooks/qlty-check.py" --tool claude \
    > "${tmp}/notrepo.json" 2> "${tmp}/notrepo.err"

test ! -s "${tmp}/notrepo.err"
python3 - <<'PY' "${tmp}/notrepo.json"
import json
import sys
payload = json.loads(open(sys.argv[1]).read())
assert payload["decision"] == "block"
assert "could not resolve repository root" in payload["reason"]
PY

echo "agent hook config tests passed"
