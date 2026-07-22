# Qlty Agent Stop Hooks

Offcut uses Qlty for repository quality checks and wires project-scoped agent
stop hooks for Codex, Pi, and Claude Code. The shared implementation is
`.qlty/hooks/qlty-check.py`; each agent-specific config calls that wrapper from
the repository root and leaves user/global configuration untouched.

## Qlty

Run the same command the hooks run:

```sh
qlty check --no-progress --no-upgrade-check
```

Qlty was initialized with `.qlty/qlty.toml`. The `.qlty/.gitignore` keeps Qlty
cache/plugin churn out of git while allowing checked-in config and hooks.

The wrapper distinguishes repository problems from environment problems:

- **Qlty reported issues**, `.qlty/qlty.toml` is missing, or the repository root
  cannot be resolved because the hook ran outside a repository: the stop is
  blocked, because the agent can act on all three from inside the repository.
- **`qlty` or `git` is not installed or not on PATH**: the stop is *not*
  blocked. The wrapper prints the reason on stderr and exits non-zero, which
  every supported tool treats as a non-blocking hook error, so the message is
  visible without holding the session open. Contributors missing either tool are
  never asked to change committed hook configuration to get their agent to stop.

The two cases are separate exception types (`RootResolutionError` versus
`ToolUnavailableError`) rather than a parsed message, so every future failure
path has to pick a side deliberately.

## Codex

Project configuration lives in `.codex/hooks.json`, and that file is the whole
project-scoped mechanism. Offcut deliberately ships **no** `.codex/config.toml`:
Codex's `hooks` feature is a *stable* feature that is already enabled by
default, and Codex discards project-local `[features]` entirely, so a committed
`[features] hooks = true` would be a no-op that misrepresents how the hook is
turned on. Only the user-level `~/.codex/config.toml` can change the flag, and
Offcut never writes there.

Codex project hooks require both project trust and *persisted hook trust*
before they run. Once trusted, the `Stop` hook invokes:

```sh
python3 "$(git rev-parse --show-toplevel)/.qlty/hooks/qlty-check.py" --tool codex
```

The wrapper reads Codex hook JSON on stdin. If `qlty check` passes, it exits
quietly. If Qlty fails, it returns a `decision: "block"` JSON response with the
captured Qlty output so Codex continues with useful feedback. When Codex reports
`stop_hook_active: true`, the wrapper allows the stop to prevent recursive
continuation loops.

Installed smoke evidence, Codex CLI `0.142.5`, all runs against an isolated
`CODEX_HOME` (no global `hooks.json`, no global `[features]`):

- `codex features list` with an empty isolated `config.toml` reports `hooks
  stable true` — no enablement step is needed.
- A project `.codex/config.toml` setting `[features] hooks = false` still
  reported `true`, and setting `hooks = true` against a user-level `hooks =
  false` still reported `false`. Project-local `[features]` has no effect in
  either direction.
- End to end: an isolated home holding only auth plus `[projects."<path>"]
  trust_level = "trusted"`, against a project with `.codex/hooks.json` and no
  `.codex/config.toml`, run with `codex exec --dangerously-bypass-hook-trust`,
  printed `hook: Stop` / `hook: Stop Completed` and ran the hook with the
  project root as cwd. Project trust alone was *not* sufficient — see the
  limitations below.

Exact limitations for this build: the identical run *without*
`--dangerously-bypass-hook-trust` did not fire the hook and printed no warning,
so an unreviewed `.codex/hooks.json` is silently skipped in non-interactive
`codex exec` until hook trust is persisted interactively. Adding unsupported
top-level keys to `.codex/hooks.json` can likewise make the file silently
ignored, so keep the top level to `hooks`.

## Claude Code

Project configuration lives in `.claude/settings.json`. The hook uses Claude
Code exec form:

```json
{
  "type": "command",
  "command": "python3",
  "args": ["${CLAUDE_PROJECT_DIR}/.qlty/hooks/qlty-check.py", "--tool", "claude"]
}
```

Claude Code substitutes `${CLAUDE_PROJECT_DIR}` and also passes hook JSON on
stdin. Failure handling mirrors Codex: a Qlty failure blocks the stop once with
the Qlty output as the continuation reason, then `stop_hook_active: true`
prevents a loop.

Installed smoke evidence: Claude Code `2.1.217` ran a temporary repo with
project-only settings and `--include-hook-events --output-format stream-json
--verbose`; the `Stop` hook emitted `hook_started` and `hook_response` events,
and the stubbed `qlty` saw the same repo-root command and recursion env as
Codex.

## Pi

Project configuration lives in `.pi/extensions/qlty-stop-hook.ts`. Pi does not
have a Codex/Claude-style `Stop` event; its current project-scoped mechanism is
a trusted project extension that listens for `session_shutdown`, which fires
with `reason` values `quit | reload | new | resume | fork`. The extension only
acts on `reason === "quit"`, so `/new`, `/fork`, `/resume`, and `/reload` are
not delayed by a full `qlty check`.

The extension runs the shared wrapper with `--tool pi --cwd <ctx.cwd>`. Because
that call is synchronous and the TUI is already gone, the extension first prints
a one-line progress notice to stderr so a quit that waits on a cold plugin cache
does not look like a frozen terminal. Pi shutdown hooks cannot force another
model turn, so failures are reported on stderr too. Pi's interactive quit path
stops the TUI *before* emitting
`session_shutdown` (so extension cleanup cannot repaint the final frame), which
means `ctx.ui.notify` is invisible on exactly the path this hook runs; stderr is
therefore the reporting channel, and `notify` is only a best-effort extra for
non-TUI front ends.

Installed smoke evidence: Pi `0.81.1` loaded a temporary project-local extension
after `--approve`, emitted `session_shutdown` in print mode even when the model
call stopped early for missing credentials, and the Offcut extension invoked the
shared wrapper from the temporary repo root with `OFFCUT_QLTY_STOP_HOOK_ACTIVE=1`.
The current Pi lifecycle API exposes shutdown cleanup hooks, not a Codex/Claude
style block-and-continue stop decision.

## Smoke Checks

Run the hook/config fixture tests with:

```sh
./tests/agent_hooks_config.sh
```

The script validates the Codex, Claude Code, and Pi project config shapes, then
exercises `.qlty/hooks/qlty-check.py` with stubbed `qlty` binaries for success,
failure, root resolution, the `stop_hook_active` recursion guard, the non-JSON
(`--tool pi`) stderr path for a repository with no `.qlty/qlty.toml`, and the
missing-binary paths where `qlty` (and then `git`) are absent from `PATH`.
