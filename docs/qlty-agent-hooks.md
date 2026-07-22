# Qlty Agent Stop Hooks

Offcut uses Qlty for repository quality checks and wires project-scoped agent
stop hooks for Codex, Pi, and Claude Code. The shared implementation is
`.qlty/hooks/qlty-check.py`; each agent-specific config calls that wrapper from
the repository root and leaves user/global configuration untouched.

## Qlty

Run the same command the hooks run:

```sh
qlty check --no-progress --no-upgrade-check --print-errors
```

`--print-errors` is not cosmetic: linter *errors* (plugin install failure, a
cold-cache network failure, an unsupported runtime) also exit non-zero, but
their detail is not written out by default. Without it the wrapper could hand an
agent a blocking reason that says only `qlty check exited with code N`. It is
also what makes the skip-versus-block split machine-readable — see the exit
taxonomy below.

Qlty was initialized with `.qlty/qlty.toml`. The `.qlty/.gitignore` keeps Qlty
cache/plugin churn out of git while allowing checked-in config and hooks.

The plugin set covers the hook implementation itself, not just Offcut's Rust
sources: `ruff` for `.qlty/hooks/qlty-check.py`, `biome` for
`.pi/extensions/qlty-stop-hook.ts`, and `shellcheck` for
`tests/agent_hooks_config.sh`. A gate that cannot see its own wrapper would
report clean while the wrapper rots, so `tests/agent_hooks_config.sh` asserts
those three plugins stay enabled.

Those three carry `version =` pins (`ruff 0.14.6`, `biome 1.9.4`,
`shellcheck 0.11.0` — the versions this config was verified against). They gate a
*blocking* stop hook, so an upstream release that adds a rule or changes
formatter defaults would otherwise start blocking every stop in this repository
with no commit here; `biome` formats `.claude/settings.json` and
`.codex/hooks.json`, so it could block on the hook configs themselves. Refresh a
pin deliberately, in a commit, and re-run the check. The fixture suite asserts
the pins exist. Security scanners are left floating on purpose: for those,
stale is worse than surprising.

The wrapper distinguishes repository problems from environment problems:

- **Qlty reported issues**, `.qlty/qlty.toml` is missing, or the repository root
  cannot be resolved because the hook ran outside a repository: the stop is
  blocked, because the agent can act on all three from inside the repository.
- **`qlty` or `git` is not installed or not on PATH**, `qlty check` outran its
  time budget, or `qlty check` could not produce a report at all: the stop is
  *not* blocked. The wrapper prints the reason on stderr and exits non-zero,
  which every supported tool treats as a non-blocking hook error, so the message
  is visible without holding the session open. Contributors missing either tool
  are never asked to change committed hook configuration to get their agent to
  stop.

The last case is classified from two Qlty signals, not one. Verified against
`qlty 0.636.0`:

| Exit | Meaning | Wrapper |
| --- | --- | --- |
| `0` | nothing to report | allow the stop |
| `1` | findings only | block with the findings |
| `3` | at least one linter errored, with or without findings | fail open |
| `99` | Qlty could not report at all: unknown plugin, install 404, no Qlty setup | fail open |

The wrapper keys on "exit code 1" rather than enumerating error codes, so a
future Qlty error code still fails open instead of blocking on findings that do
not exist. Exit code alone is not enough, though: `qlty check --help` documents
`--no-error` as "Exit successfully regardless of linter errors", so a linter
failure is in principle part of the same failure path as findings. The wrapper
therefore also reads the per-invocation `exitResult` that `--print-errors`
writes — the `qlty.analysis.v1` enum, whose error variants all end in `_ERROR` —
and fails open whenever an invocation errored, even at exit `1`. Matching the
suffix rather than one variant name keeps a future error variant on the
fail-open side; matching `exitResult` rather than the presence of a dump matters
because `--print-errors` also dumps `EXIT_RESULT_SUCCESS` invocations for
linters that merely exited non-zero to report issues.

`qlty check` runs under a wrapper-owned budget (540s by default, overridable
with `OFFCUT_QLTY_CHECK_TIMEOUT_SECONDS`) that is deliberately under each host's
600s hook timeout. The host kills only the direct `python3` child, which would
strand the `qlty` grandchild — for this config a full `clippy`/`cargo` build —
burning CPU and holding the Qlty cache after the agent moved on. The wrapper
therefore starts `qlty` in its own session and, on timeout, signals the whole
process group (SIGTERM, then SIGKILL) before reporting a distinct
`qlty check timed out` message.

The two cases are separate exception types (`RootResolutionError` versus
`ToolUnavailableError`) rather than a parsed message, so every future failure
path has to pick a side deliberately.

Root resolution is bounded by the project the wrapper is installed in. The
wrapper ships at `<root>/.qlty/hooks/qlty-check.py`, so its own location names
that project exactly — the same principle the Pi extension and the fixture
script use. Nothing above it can ever be selected as the root.

Within that boundary it asks `git rev-parse --show-toplevel` first, accepting the
answer only when the named root is inside the project *and* owns
`.qlty/qlty.toml`; otherwise it walks the hook's cwd upward, no further than the
project root, for the nearest directory that does. Each half fixes a real shape:

- A scratch repository `git init`ed inside the worktree — routine for a tool
  whose job is walking git projects — would otherwise be named as the root and
  block every stop with a "restore the Qlty config" instruction that would
  scatter a stray `.qlty/` into it.
- An unbounded walk would climb past the checkout. `~/.qlty/` is the directory
  the Qlty installer itself creates, so a `~/.qlty/qlty.toml` is a plausible
  ancestor hit; adopting it would run `qlty check` over the whole home tree and
  report unrelated projects' findings back to the agent.

When no directory inside the project owns a config, the git root is still what
the misconfiguration message names, so a genuinely Qlty-less repository blocks
exactly as before.

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
python3 "$(git rev-parse --show-toplevel 2>/dev/null || pwd)/.qlty/hooks/qlty-check.py" --tool codex
```

The `|| pwd` fallback keeps the wrapper reachable. An unguarded substitution
collapses to the empty string when `git` is missing or the cwd is not a
worktree, so Codex would run `python3 "/.qlty/hooks/qlty-check.py"` and die with
a raw interpreter error — bypassing the whole skip-versus-block taxonomy above.

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

The extension is loaded from `<root>/.pi/extensions/qlty-stop-hook.ts`, so its
own location names the repository root exactly; it resolves the root two
directories up from `import.meta.dirname` rather than shelling out to `git`.
That removes a subprocess from the quit path and a failure mode: when `ctx.cwd`
sits inside a nested or unrelated repository, `rev-parse` names the wrong root
and the hook silently degrades to the "was not found" warning.

The extension runs the shared wrapper with `--tool pi --cwd <repo root>`. Because
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

The fixture suite runs as part of the ordinary test run — `tests/agent_hooks_config.rs`
is a thin harness that shells out to the script — so config drift fails
`cargo test` rather than waiting for someone to remember a manual command:

```sh
cargo test --test agent_hooks_config
```

Run it directly while iterating:

```sh
./tests/agent_hooks_config.sh
```

The harness is `#![cfg(unix)]`, so it compiles out on Windows. The script needs
`bash`, `git`, and a `python3` of 3.11 or newer — it reads `.qlty/qlty.toml`
(and any `.codex/config.toml`) with `tomllib`. Apple's system `python3` is still
3.9, so on macOS a stock `cargo test` fails on that import rather than on real
drift; put a newer `python3` first on `PATH`. The wrapper itself has no such
floor.

The script derives the repository root from its own location rather than from
`git rev-parse`, for the same reason the Pi extension does: it has to work in a
source tarball with no `.git` and inside a checkout vendored under another
repository.

It validates the Codex, Claude Code, and Pi project config shapes plus the
plugin coverage and version pins above, then exercises the wrapper with stubbed
`qlty` binaries. Because root resolution is bounded by the project the wrapper
is installed in, the fixtures copy it to `<fixture root>/.qlty/hooks/` and run
that copy, which is how it ships.

Covered: success; an exit-1 findings failure that blocks; exit-3 and exit-99
failures that fail open for all three tools; an errored `exitResult` at exit 1
that fails open even though the exit code says findings; root resolution from a
nested git repository that owns no Qlty config; refusal to adopt an ancestor
config from outside the project; a directory that sits in no repository at all,
which blocks with `could not resolve repository root`; the `stop_hook_active`
recursion guard; the non-JSON (`--tool pi`) stderr path for a repository with no
`.qlty/qlty.toml`; the missing-binary paths where `qlty` (and then `git`) are
absent from `PATH`; and a timeout whose stub spawns a grandchild — asserting
both the distinct non-blocking message and that the grandchild died with the
killed process group.
