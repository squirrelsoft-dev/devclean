# offcut

Development environment cleanup CLI.

The binary loads configuration, discovers projects, classifies their git
state, and cleans the cleanable ones. `offcut list` prints each project's
status sorted by severity without cleaning; `offcut config` prints the
resolved configuration (preserved from the original list-of-resolved-config
behavior); `offcut` (no subcommand) and `offcut clean` each run the
interactive clean flow — sorted report, each cleanable project enumerated,
interactive prompts, and deletion. Each command is documented below.

## Build

```sh
cargo build
```

## Run

```sh
cargo run -- --version
cargo run -- list
cargo run -- clean
```

## Test

```sh
cargo test
```

## Release

`Cargo.toml`'s package `version` is the release version source of truth. To
release, merge the version bump, create a tag named `vX.Y.Z` from that commit,
and push the tag. The tag-triggered GitHub Actions release workflow verifies
that the tag exactly matches `Cargo.toml`, runs `cargo test --locked`, builds
stripped archives for macOS arm64, macOS x86_64, Linux x86_64, and Linux
aarch64, writes SHA256 checksums, and publishes a GitHub Release with generated
notes.

Release archives are named `offcut-vX.Y.Z-<target>.tar.gz` and each has a
matching `.sha256` file. macOS binaries are currently unsigned and unnotarized.

## CLI surface

Each subcommand is optional — running with no subcommand runs the default
interactive clean flow.

| command | behavior |
|---------|----------|
| `offcut` | default run: discover → classify → report each project sorted by status → interactive clean for each cleanable project |
| `offcut list` | read-only: show each project's git status, sorted by severity (most-needs-attention first). No cleaning. |
| `offcut config` | print the resolved configuration (workspace roots, max_depth, default_mode, each invocation's flags) |
| `offcut clean` | interactive: report each project sorted by status, each cleanable project enumerated, prompts, deletion. Destructive. Accepts an optional `<PROJECT_PATH>` to scope discovery and deletion strictly to that one project. |
| `offcut discovery` | walk each workspace root and report discovered projects (paths + markers) |
| `offcut classification` | classify each discovered project by its git state, print each status |
| `offcut ignore <path>` | test whether `path` is ignored by the loaded `.offcutignore` |
| `offcut safelist <path>` | test whether `path` is safe to delete according to the catalog |
| `offcut init <path>` | create a TOML config file pre-populated with each `Config` field's default and the given workspace root active; idempotent (does not clobber an existing file) |

### Top-level flags

Each flag is top-level and must be given *before* the subcommand:

```sh
offcut [FLAGS] list              # list projects + statuses
offcut [FLAGS] clean [PROJECT_PATH]  # interactive clean flow (destructive); PROJECT_PATH scopes to one project
offcut [FLAGS] config            # print the resolved config

  --workspace <path>               # append a workspace root (repeatable)
  --config <path>                  # alternate config file (must exist; `init` creates it)
  --force                          # skip each prompt, auto-approve each surfaced item (destructive)
  --dry-run                        # show each item's fate, delete nothing
  --verbose                        # verbose output
  --version                        # print the crate version
```

`--force` and `--dry-run` may be combined: `--force --dry-run` previews the
force run without deleting. Runtime precedence lives in one place each:
`dry_run` alone gates execution, `force` alone gates
prompting/auto-approval.

For each cleanable project, `offcut clean` enumerates untracked items,
prompts each `Surfaced` item, asks for a final `[y/N]` confirmation for the
project, then executes `git clean -xfd -e <globs>` if approved. `--force`
skips each prompt and auto-approves each surfaced item. `--dry-run` previews
each item's fate and deletes nothing.

```sh
$ offcut                       # default run: list + interactive clean
$ offcut list                  # read-only project listing
$ offcut clean                 # destructive interactive flow
$ offcut clean ~/code/looper   # clean ONLY the named project (shell expands ~)
$ offcut --force clean         # DESTRUCTIVE: each prompt skipped
$ offcut --dry-run clean       # preview only; each item printed
$ offcut --force --dry-run clean  # preview of the force run; deletes nothing
$ offcut --verbose clean       # verbose output
$ offcut --version             # crate version (offcut 0.1.0)
```

To get started: `offcut init ~/code` writes a pre-populated config file under
the platform config dir with `~/code` as the active workspace root, each other
`Config` field documented with its default, and a comment explaining how to add
more workspace roots. `offcut init --help` for details.

See `src/main.rs` for the CLI surface and `src/output.rs` for the
formatting; `src/clean.rs` for the deletion engine and
`src/interactive.rs` for the approval state machine.

### Colors

Output is colored when stdout is a TTY and plain when piped or redirected.
Setting the `NO_COLOR` environment variable (to any value) or `CLICOLOR=0`
disables color even on a TTY. Known limitation: offcut emits standard ANSI
escapes and does not enable virtual-terminal processing on legacy Windows
conhost (plain `cmd.exe`), where colored output may render as escape
sequences — modern Windows Terminal, macOS, and Linux terminals are fine.

### Terminal interface

On a capable terminal, `offcut list`, the default `offcut` run, and
`offcut clean` use a compact terminal UI modeled around four states:

- **Working** — discovery, classification, sizing, and cleaning render as a
  single live spinner line with the current path and, where known, an `N/M`
  counter. Press Ctrl-C to cancel; no partially approved delete is resumed.
- **Workspace summary** — after discovery/classification, projects are shown
  in a table with the project path (abbreviated with `~`, and truncated from
  the left so the leaf stays readable), status, reclaimable size, branch, and
  last commit age where git can report it. Narrow terminals fall back to
  stacked rows so paths and labels do not wrap into adjacent columns.
- **Clean review** — each cleanable project has a review panel showing its
  branch state and every gitignored path Offcut would remove or ask about,
  each with its classification and its fate. The panel is printed
  immediately before the real `[y/N]` approval prompt it belongs to and asks
  nothing itself, so the destructive confirmation is asked exactly once. In
  `--force` / `--dry-run` runs, where no question is asked, the panel is
  printed with the fate each item actually gets (`deleting`, `would delete`,
  `would prompt`, `kept`).
- **Blocked** — a targeted `offcut clean <PROJECT_PATH>` against a
  non-cleanable project shows the refusal reason, relevant `git status`
  detail for WIP trees, and the command to rerun after the project is clean
  and pushed.

When stdout is piped, redirected, or `TERM=dumb`, Offcut keeps the plain
line-oriented output and suppresses live progress. Color is independently
disabled by `NO_COLOR` or `CLICOLOR=0`; the safety model and exit behavior do
not depend on terminal styling.

## `.offcutignore`

offcut honors gitignore-style ignore files in two scopes:

- **Global** — `~/.offcutignore` applies to every project. Its patterns are
  anchored at the project root (like git's `core.excludesfile`).
- **Per-folder** — a `.offcutignore` file inside a project tree applies to
  the subtree rooted at its own directory, at any depth.

Patterns use gitignore semantics: `*` and `**` globs, a leading `/` that
anchors to the ignore file's directory, a trailing `/` for directory-only
matches, and `!` to re-include a previously excluded path. Blank lines and
lines starting with `#` are ignored. Matching a directory also covers
everything inside it, so `build/` protects `build/out.o` too.

Precedence follows gitignore: the closest (deepest, most-specific)
`.offcutignore` wins, layered on top of the global file. A path matched by
an ignore rule is **protected** — cleaning never removes it. A `!` pattern
**un-protects** (re-includes) a path an earlier rule excluded, per gitignore
semantics: a `!`-whitelisted path is NOT protected and is eligible for
cleaning (subject to the safe-list and approval rules below).

Example `~/.offcutignore`:

```gitignore
# global: never touch these anywhere in a project
.DS_Store
*.swp
/secrets
```

A per-folder `<project>/sub/.offcutignore`:

```gitignore
# ignore this subtree's build output
build/
# ...but keep one debug log
!debug.log
```

There is a small debug helper to inspect the loaded rules:

```sh
$ offcut ignore path/to/check
path/to/check: ignored
```

It treats the current directory as the project root, loads the global file plus
every `.offcutignore` beneath it, and tests the given path. The path may be
relative to the current directory or absolute inside it; a path outside the
project root is an error rather than a reported "not-ignored".

## Cleaning

offcut cleans each **cleanable** (status-5) project by enumerating its
untracked items, classifying each, surfacing the ambiguous ones for approval,
then deleting via `git clean -xfd -e <exclusion globs>` — mirroring the
original zshrc approach (build an exclusion list, run `git clean`).

Each untracked item is classified into one of three classes:

| class        | condition                       | outcome              |
|--------------|---------------------------------|----------------------|
| `protected`  | `.offcutignore` match         | never removed        |
| `safe`       | safe-to-delete catalog match    | auto-removed         |
| `surfaced`   | everything else                 | per-project approval |

Untracked items are enumerated *without* git's `--exclude-standard`, so
project-gitignored build junk like `node_modules`/`target/` stays visible —
offcut exists to clean it, and `.offcutignore` is the sole judge of
protection. The exclusion list passed to `git clean -e` is built from every
protected item plus every surfaced item the user does **not** approve; safe
items and approved surfaced items are left un-excluded so `git clean` deletes
them. An absolute path on the cleaning path is treated as **protected**
(fail-safe toward not-deleting, never fail-open).

`offcut clean` runs the interactive flow (#8): it reports every project
sorted by status (statuses 1–4 need manual attention; status-5 projects are
the cleanable subjects), asks "Remove gitignored paths from the N cleanable
projects? [y/N]", then loops over the cleanable projects one at a time —
showing the items that would be deleted *before* asking anything, prompting
per surfaced item, and prompting per project — and finally deletes the
approved items. Answering "n" (or EOF) to the first prompt exits without
touching anything.

Flags:

- `--force` — **destructive**: skip every prompt and clean all cleanable
  projects, auto-approving every surfaced item.
- `--dry-run` — non-destructive preview: print each item's fate and
  delete nothing. Safe items are shown as `would delete`; surfaced items as
  `would prompt` (a real interactive run asks about them). Combined with
  `--force` it previews the force run: every non-protected item is shown as
  `would delete`, and still nothing is deleted.
- `--verbose` — accepted and echoed in the `offcut config` flags line;
  currently produces no additional output elsewhere.

### Disk savings (issue #18)

Each cleanable (status-5) project row carries a reclaimable-size estimate
next to the path: `path — cleanable (~2.3 GB)`. The estimate is computed
from the items the cleaning engine would delete — every `Safe` item (the
built-in catalog) plus every `Surfaced` item (in `dry_run` mode every
surfaced item is auto-approved). `Protected` items match `.offcutignore`
(or the absolute-path fail-safe), so they are never counted.

The aggregate across all cleanable projects is shown in the summary line:
`listing: N project(s), M cleanable, ~X reclaimable`. The `~` form makes it
an estimate — the walk tolerates permission-denied paths (counted as 0) and
non-UTF-8 names without aborting the listing. Sizes are 1024-based
KB / MB / GB and each symlink counts its own size only (no target recursion).
Each file's size comes from its metadata (`stat`, O(1) per file) — contents
are never read, so sizing stays fast even for large build artifacts.

Both `offcut list` and `offcut clean` (including `--dry-run` and
`--force --dry-run`) carry the same per-project and total sizes — the
enumeration is the same, the display is the same.

Statuses 1–4 (manual-attention) show no reclaimable size — each is not yet
cleanable. Only status-5 rows and the aggregate carry the savings.

```sh
$ offcut clean                    # interactive: report, approve, then delete
$ offcut clean ~/code/looper      # clean ONLY the named project (shell expands ~)
$ offcut --dry-run clean          # preview only; deletes nothing
$ offcut --force clean            # DESTRUCTIVE: each prompt skipped
$ offcut --force --dry-run clean  # preview of the force run; deletes nothing
$ offcut --verbose clean          # verbose output
```

Each subcommand — `offcut` (no subcommand, the default run) and
`offcut clean` — runs the destructive interactive flow; only `offcut
list` lists each project's status without cleaning.

### Project-path targeting

`offcut clean <PROJECT_PATH>` scopes discovery and deletion strictly to the
named project — no other project is discovered or cleaned, even if it sits
inside a configured `workspace_roots` entry. The path may be absolute or
relative to the current directory, and is resolved to an absolute,
symlink-resolved path — that resolved form is what the report shows. The
caller's shell is expected to expand `~` (offcut does not perform tilde
expansion itself), so the normal usage is:

```sh
$ offcut clean ~/Developer/squirrelsoft-dev/looper
$ offcut clean ./relative/path/to/project
$ offcut --force --dry-run clean ~/code/looper   # preview the targeted clean
```

Every top-level flag (`--force`, `--dry-run`, `--config`) still applies to
the targeted project, unchanged by path targeting (`--verbose` is accepted
here too, and stays as inert as it is everywhere else). `--workspace` is
accepted but ignored in this mode — the explicit path alone drives the run,
so a project outside any configured workspace root can still be cleaned.
When `--workspace` is combined with a `PROJECT_PATH`, offcut emits one
concise stderr notice that the path scopes the run and `--workspace` is
ignored, so a mistyped invocation is not mistaken for a wider run. The
targeted project still flows through classification: a non-cleanable project
(e.g. one with uncommitted work) is reported as such and not cleaned. A
missing or non-directory path is a hard error; when such a path still begins
with a literal `~` (the shell did not expand it — say it was quoted), the
error carries a hint pointing at shell tilde expansion.

`<PROJECT_PATH>` must be the **project root**. A path inside a git project —
say `~/code/looper/crates/inner` — is rejected before anything is deleted,
with an error naming the enclosing root:

```
offcut: project path is not a project root: /Users/me/code/looper/crates/inner
it is inside the git project at /Users/me/code/looper — pass that path instead
```

This mirrors discovery's nesting rule (a folder inside a git worktree is a
subfolder of that project, not a project). Classification would refuse such a
path anyway — a subdirectory has no `.git` of its own, so it is reported
`no-git`, which is never cleanable — so the root check is a second, earlier
guard that fails with an actionable error instead of a confusing status. A
nested repository — a submodule or a vendored clone — *is* its own root, so
it may be targeted directly. A directory that is not inside any git project
is still accepted and reported by classification (as `no-git`) rather than
silently dropped.

See `src/clean.rs` for the deletion engine and `src/interactive.rs` for the
approval state machine (the `clean` / `dry_run` / `build_exclusions` API is
the seam the interactive flow drives).

## Safe-to-delete catalog

offcut keeps a catalog of paths that are safe to auto-delete — the ones a
cleaning run may remove without asking. `offcut safelist <path>` reports
whether a given path is in it. The catalog is:

- **Built-in defaults** — compiled into `src/safelist.rs` as `BUILT_IN_DEFAULTS` (a non-exhaustive list of common build/cache dirs/files: `node_modules`, `target`, `.next`, `.turbo`, `dist`, `build`, `__pycache__`, `.venv`, `venv`, `.pytest_cache`, `.mypy_cache`, `.gradle`, `bin/obj`, `out`, `coverage`, `.nuxt`, `.svelte-kit`, `.cache`, `.parcel-cache`).
- **Extension via `Config::safe_delete`** — user-supplied gitignore-style globs are appended (not replaced) to the built-in set. Patterns behave like gitignore globs anchored at the project root: each matches the named dir at any depth (e.g. `**/node_modules`), and matching a directory covers everything beneath it via the `ignore` crate's parent-match semantics.
- **Intended consumers** — the cleaning engine, which removes matched paths
  without asking for approval (see "Cleaning" above), and the discovery walk,
  which derives its descent-prune set from the same catalog (see "Discovery"
  below).
- **Observable hook** — `offcut safelist <path>` is the minimal diagnostic
  for the catalog.

A path is reported `safe` or `not-safe`, interpreted relative to the current
directory (treated as the project root); a path outside that root is an error
rather than a reported "not-safe". Directory-only patterns (a trailing `/`) are
matched against the path's actual kind on disk, so a plain file named `build`
is not reported safe by a `build/` pattern.

See `src/safelist.rs` for the implementation; `tests/safelist_cli.rs` for integration tests of the subcommand.

## Discovery

`offcut discovery` walks each configured `workspace_roots` entry up to
`max_depth` and reports every folder that contains one of the configured
`project_markers` (`.git`, `package.json`, `Cargo.toml`, `go.mod`,
`pyproject.toml`, `pom.xml`, `build.gradle`, `*.csproj`, or any user-supplied
additions). Markers may be exact filenames or glob patterns; a folder is
reported once, tagged with one of the markers found in it.

Depth is counted from each workspace root: depth 0 is the root itself, depth 1
a direct child, and so on. The walk never rises above a root, symlinks are
not followed, and `WalkDir` is instructed via `filter_entry` to never descend
into a `.git` directory (git internals are never candidate projects). The
safelist catalog (`BUILT_IN_DEFAULTS` plus each user-supplied `safe_delete`
entry) is the single source of truth for what is a non-project artifact: each
`**/<single-segment>` pattern — or an equivalent bare `<single-segment>`
entry — contributes its basename to the descent-prune set, so
`node_modules`, `target`, `dist`, `build`, `.next`, `.venv`, and other
built-in junk dirs are not walked at all. Multi-segment patterns like
`**/bin/obj` remain clean-time-only (not over-pruned on bare `obj`). A
project's `.git` marker is still detected via `fs::read_dir` on its own
children — only descent into `.git/` internals is skipped. Each git project
yields at most one entry: any marker in a subfolder of an ancestor git
worktree — a non-git marker (`Cargo.toml`, `package.json`, ...) or a nested
`.git`, whether a directory or a worktree/submodule gitdir pointer file — is
part of that project, not a new one. A monorepo with a root `.git` and
marker-bearing package folders yields one entry, and a repo's nested git
worktrees are not reported separately. A non-git marker at a workspace root
is always reported; a git repo above the root is never consulted. Reported
paths are printed as they were walked from the workspace root, so configuring
absolute roots (the usual case) yields absolute output.

```
offcut discovery                        # uses workspace_roots from config
offcut --workspace ~/code discovery     # append a root from the CLI
```

With no workspace roots configured, discovery reports that and exits without
error. An unreadable or missing root is a hard error rather than a silent
empty result.

Each subcommand that runs discovery — `offcut discovery`, `offcut list`,
`offcut classification`, and the default run / `offcut clean` — renders a
live single-line progress indicator while walking each workspace root:
`<spinner> walking: <path>` overwrites itself in place via a carriage return
on a capable TTY, so the display never scrolls. (`offcut clean
<PROJECT_PATH>` bypasses the walk entirely, so it renders no `walking:` line
and starts at the phases below.) The same indicator continues through the
later phases: while each project's git state is examined the line reads
`<spinner> classifying N/M: <path>`, while each cleanable project's
reclaimable size is computed the line reads `<spinner> sizing N/M: <path>`
(single-pass — bytes are stored once and the aggregate is the sum of those
already-computed bytes, not a second walk), while the branch and last-commit
columns of the summary table are read from git it reads
`<spinner> reading N/M: <path>`, and while an approved project's untracked
junk is deleted it reads `<spinner> cleaning N/M: <path>` (N is the 1-based
project counter, M the total). When stdout is piped, redirected, or
`TERM=dumb`, nothing is rendered — a stream of CR-terminated partial paths
would be garbage in a pipe or log file. Terminal width is resolved via the
`terminal_size` crate, falling back to `COLUMNS` then a default of 80; paths
that would wrap are truncated with a leading ellipsis so the leaf (current
directory) stays visible. Whenever regular output must interleave with the
indicator (per-project rows, warnings), the progress line is first cleared in
place; when a phase completes, it is cleared and a newline is emitted so the
following summary line starts on a fresh line.

See `src/discovery.rs` for the matching and nesting rules;
`src/progress.rs` for the live indicator; `tests/discovery_cli.rs` for
integration tests of the subcommand; `tests/progress_cli.rs` for
integration tests of the classification/cleaning/sizing progress phases.

## Classification

`offcut classification` runs discovery, then classifies each project by
its git state and prints the results sorted by severity (most-needs-attention
first). Only status 5 is cleanable; statuses 1-4 each mean cleaning must wait.

| # | Status | Meaning |
|---|-------|---------|
| 1 | `no-git` | Has a project marker but is NOT git-initialized (or `.git` is a dangling gitdir pointer to a path that no longer exists). |
| 2 | `no-remote` | Git repo with no remote configured. |
| 3 | `unpushed` | Git repo with a remote but unpushed commits (or no upstream). |
| 4 | `wip` | Git repo with uncommitted work-in-progress (modified/staged tracked changes). |
| 5 | `cleanable` | Committed + pushed, AND has untracked junk that is NOT offcutignored. |
| 6 | `clean` | Committed + pushed with no untracked non-offcutignored junk. Not a dirty status; sorts last. |

Precedence is the lowest-numbered (most-severe) status. A repo with both WIP
(4) and untracked junk is status 4, not 5 — it must not be cleaned while it has
uncommitted work. The `.offcutignore` matcher (see above) decides whether
untracked junk is protected, which separates `cleanable` from `clean`.

```
offcut classification                 # uses workspace_roots from config
offcut --workspace ~/code classification
```

See `src/classify.rs` for the git-plumbing logic and status precedence;
`tests/classification_cli.rs` for integration tests. The interactive
cleaning flow that acts on these statuses is `offcut clean` (see
"Cleaning" above).

## Configuration

offcut reads a TOML config file from the platform config dir
(`~/.config/offcut/config.toml` on Linux,
`~/Library/Application Support/offcut/config.toml` on macOS,
`%APPDATA%\offcut\config.toml` on Windows). A missing file at that default
location is not an error: built-in defaults are used. A path you pass explicitly
with `--config` must exist — offcut exits non-zero rather than silently
falling back to defaults. The one exception is `offcut init`, which creates
the file (see below).

First run? `offcut init <path>` writes a pre-populated config file under the
platform config dir with the given path as the active workspace root, each
other `Config` field documented with its default, and a comment explaining how
to add more workspace roots. `init` refuses to overwrite an existing file at
the target — it exits non-zero with the existing path, and does not read or
use that file. `offcut --config <other>` writes to `<other>` instead
(creating parent directories); the same refuse-to-overwrite rule applies to an
explicit `--config` target.

```toml
# ~/.config/offcut/config.toml
workspace_roots = ["/home/me/code", "/home/me/work"]
safe_delete = ["**/my_build_artifact"]   # added to the built-ins, not a replacement
max_depth = 4
project_markers = [".git", "package.json", "Cargo.toml", "go.mod", "pyproject.toml", "pom.xml", "build.gradle", "*.csproj"]
default_mode = "interactive"   # or "force"
```

Defaults:

- `workspace_roots` — `[]`
- `safe_delete` — `[]` (extra patterns *added* to the built-in catalog; see
  "Safe-to-delete catalog" above, which applies even when this is empty)
- `max_depth` — `4`
- `project_markers` — the list shown above
- `default_mode` — `interactive`

### CLI flags (override config)

Each flag is top-level and must be given *before* the subcommand:

```sh
offcut [FLAGS] list              # read-only: each project's git status
offcut [FLAGS] config            # print the resolved config (preserved)
offcut [FLAGS] clean [PROJECT_PATH]  # destructive: interactive flow, each prompt; PROJECT_PATH scopes to one project
offcut [FLAGS] discovery         # each project with markers
offcut [FLAGS] classification    # each project classified, sorted
offcut [FLAGS] ignore <path>     # see `.offcutignore` above
offcut [FLAGS] safelist <path>   # see "Safe-to-delete catalog" above
offcut [FLAGS] init <path>       # create a pre-populated config file (see above)

  --workspace <path>               # append a workspace root (repeatable)
  --config <path>                  # alternate config file (must exist; `init` creates it)
  --force                          # each prompt skipped, each surfaced item auto-approved (destructive)
  --dry-run                        # each item's fate printed, nothing deleted
  --verbose                        # verbose output
  --version                        # crate version (each build)
```

`--force` and `--dry-run` may be combined: `--force --dry-run` previews the
force run without deleting. Runtime precedence lives in one place each:
`dry_run` alone gates execution, `force` alone gates
prompting/auto-approval.

For example:

```sh
offcut --config ./offcut.toml --workspace ~/code --force clean
offcut --workspace ~/code --dry-run classification
```
