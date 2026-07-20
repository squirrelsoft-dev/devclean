# devclean

Development environment cleanup CLI.

The binary loads configuration, discovers projects, classifies their git
state, and cleans the cleanable ones. `devclean list` prints each project's
status sorted by severity without cleaning; `devclean config` prints the
resolved configuration (preserved from the original list-of-resolved-config
behavior); `devclean` (no subcommand) and `devclean clean` each run the
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

## CLI surface

Each subcommand is optional — running with no subcommand runs the default
interactive clean flow.

| command | behavior |
|---------|----------|
| `devclean` | default run: discover → classify → report each project sorted by status → interactive clean for each cleanable project |
| `devclean list` | read-only: show each project's git status, sorted by severity (most-needs-attention first). No cleaning. |
| `devclean config` | print the resolved configuration (workspace roots, max_depth, default_mode, each invocation's flags) |
| `devclean clean` | interactive: report each project sorted by status, each cleanable project enumerated, prompts, deletion. Destructive. |
| `devclean discovery` | walk each workspace root and report discovered projects (paths + markers) |
| `devclean classification` | classify each discovered project by its git state, print each status |
| `devclean ignore <path>` | test whether `path` is ignored by the loaded `.devcleanignore` |
| `devclean safelist <path>` | test whether `path` is safe to delete according to the catalog |
| `devclean init <path>` | create a TOML config file pre-populated with each `Config` field's default and the given workspace root active; idempotent (does not clobber an existing file) |

### Top-level flags

Each flag is top-level and must be given *before* the subcommand:

```sh
devclean [FLAGS] list              # list projects + statuses
devclean [FLAGS] clean             # interactive clean flow (destructive)
devclean [FLAGS] config            # print the resolved config

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

For each cleanable project, `devclean clean` enumerates untracked items,
prompts each `Surfaced` item (delete or keep), prompts each project (clean?
y/n), then executes `git clean -xfd -e <globs>` if approved. `--force` skips
each prompt and auto-approves each surfaced item. `--dry-run` previews each
item's fate and deletes nothing.

```sh
$ devclean                       # default run: list + interactive clean
$ devclean list                  # read-only project listing
$ devclean clean                 # destructive interactive flow
$ devclean --force clean         # DESTRUCTIVE: each prompt skipped
$ devclean --dry-run clean       # preview only; each item printed
$ devclean --force --dry-run clean  # preview of the force run; deletes nothing
$ devclean --verbose clean       # verbose output
$ devclean --version             # crate version (devclean 0.1.0)
```

To get started: `devclean init ~/code` writes a pre-populated config file under
the platform config dir with `~/code` as the active workspace root, each other
`Config` field documented with its default, and a comment explaining how to add
more workspace roots. `devclean init --help` for details.

See `src/main.rs` for the CLI surface and `src/output.rs` for the
formatting; `src/clean.rs` for the deletion engine and
`src/interactive.rs` for the approval state machine.

### Colors

Output is colored when stdout is a TTY and plain when piped or redirected.
Setting the `NO_COLOR` environment variable (to any value) or `CLICOLOR=0`
disables color even on a TTY. Known limitation: devclean emits standard ANSI
escapes and does not enable virtual-terminal processing on legacy Windows
conhost (plain `cmd.exe`), where colored output may render as escape
sequences — modern Windows Terminal, macOS, and Linux terminals are fine.

## `.devcleanignore`

devclean honors gitignore-style ignore files in two scopes:

- **Global** — `~/.devcleanignore` applies to every project. Its patterns are
  anchored at the project root (like git's `core.excludesfile`).
- **Per-folder** — a `.devcleanignore` file inside a project tree applies to
  the subtree rooted at its own directory, at any depth.

Patterns use gitignore semantics: `*` and `**` globs, a leading `/` that
anchors to the ignore file's directory, a trailing `/` for directory-only
matches, and `!` to re-include a previously excluded path. Blank lines and
lines starting with `#` are ignored. Matching a directory also covers
everything inside it, so `build/` protects `build/out.o` too.

Precedence follows gitignore: the closest (deepest, most-specific)
`.devcleanignore` wins, layered on top of the global file. A path matched by
an ignore rule is **protected** — cleaning never removes it. A `!` pattern
**un-protects** (re-includes) a path an earlier rule excluded, per gitignore
semantics: a `!`-whitelisted path is NOT protected and is eligible for
cleaning (subject to the safe-list and approval rules below).

Example `~/.devcleanignore`:

```gitignore
# global: never touch these anywhere in a project
.DS_Store
*.swp
/secrets
```

A per-folder `<project>/sub/.devcleanignore`:

```gitignore
# ignore this subtree's build output
build/
# ...but keep one debug log
!debug.log
```

There is a small debug helper to inspect the loaded rules:

```sh
$ devclean ignore path/to/check
path/to/check: ignored
```

It treats the current directory as the project root, loads the global file plus
every `.devcleanignore` beneath it, and tests the given path. The path may be
relative to the current directory or absolute inside it; a path outside the
project root is an error rather than a reported "not-ignored".

## Cleaning

devclean cleans each **cleanable** (status-5) project by enumerating its
untracked items, classifying each, surfacing the ambiguous ones for approval,
then deleting via `git clean -xfd -e <exclusion globs>` — mirroring the
original zshrc approach (build an exclusion list, run `git clean`).

Each untracked item is classified into one of three classes:

| class        | condition                       | outcome              |
|--------------|---------------------------------|----------------------|
| `protected`  | `.devcleanignore` match         | never removed        |
| `safe`       | safe-to-delete catalog match    | auto-removed         |
| `surfaced`   | everything else                 | per-project approval |

Untracked items are enumerated *without* git's `--exclude-standard`, so
project-gitignored build junk like `node_modules`/`target/` stays visible —
devclean exists to clean it, and `.devcleanignore` is the sole judge of
protection. The exclusion list passed to `git clean -e` is built from every
protected item plus every surfaced item the user does **not** approve; safe
items and approved surfaced items are left un-excluded so `git clean` deletes
them. An absolute path on the cleaning path is treated as **protected**
(fail-safe toward not-deleting, never fail-open).

`devclean clean` runs the interactive flow (#8): it reports every project
sorted by status (statuses 1–4 need manual attention; status-5 projects are
the cleanable subjects), asks "Clean the N cleanable projects? (y/n)", then
loops over the cleanable projects one at a time — showing the items that
would be deleted *before* asking anything, prompting per surfaced item
(delete or keep), and prompting per project — and finally deletes the
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
- `--verbose` — accepted and echoed in the `devclean config` flags line;
  currently produces no additional output elsewhere.

### Disk savings (issue #18)

Each cleanable (status-5) project row carries a reclaimable-size estimate
next to the path: `path — cleanable (~2.3 GB)`. The estimate is computed
from the items the cleaning engine would delete — every `Safe` item (the
built-in catalog) plus every `Surfaced` item (in `dry_run` mode every
surfaced item is auto-approved). `Protected` items match `.devcleanignore`
(or the absolute-path fail-safe), so they are never counted.

The aggregate across all cleanable projects is shown in the summary line:
`listing: N project(s), M cleanable, ~X reclaimable`. The `~` form makes it
an estimate — the walk tolerates permission-denied paths (counted as 0) and
non-UTF-8 names without aborting the listing. Sizes are 1024-based
KB / MB / GB and each symlink counts its own size only (no target recursion).
Each file's size comes from its metadata (`stat`, O(1) per file) — contents
are never read, so sizing stays fast even for large build artifacts.

Both `devclean list` and `devclean clean` (including `--dry-run` and
`--force --dry-run`) carry the same per-project and total sizes — the
enumeration is the same, the display is the same.

Statuses 1–4 (manual-attention) show no reclaimable size — each is not yet
cleanable. Only status-5 rows and the aggregate carry the savings.

```sh
$ devclean clean                    # interactive: report, approve, then delete
$ devclean --dry-run clean          # preview only; deletes nothing
$ devclean --force clean            # DESTRUCTIVE: each prompt skipped
$ devclean --force --dry-run clean  # preview of the force run; deletes nothing
$ devclean --verbose clean          # verbose output
```

Each subcommand — `devclean` (no subcommand, the default run) and
`devclean clean` — runs the destructive interactive flow; only `devclean
list` lists each project's status without cleaning.

See `src/clean.rs` for the deletion engine and `src/interactive.rs` for the
approval state machine (the `clean` / `dry_run` / `build_exclusions` API is
the seam the interactive flow drives).

## Safe-to-delete catalog

devclean keeps a catalog of paths that are safe to auto-delete — the ones a
cleaning run may remove without asking. `devclean safelist <path>` reports
whether a given path is in it. The catalog is:

- **Built-in defaults** — compiled into `src/safelist.rs` as `BUILT_IN_DEFAULTS` (a non-exhaustive list of common build/cache dirs/files: `node_modules`, `target`, `.next`, `.turbo`, `dist`, `build`, `__pycache__`, `.venv`, `venv`, `.pytest_cache`, `.mypy_cache`, `.gradle`, `bin/obj`, `out`, `coverage`, `.nuxt`, `.svelte-kit`, `.cache`, `.parcel-cache`).
- **Extension via `Config::safe_delete`** — user-supplied gitignore-style globs are appended (not replaced) to the built-in set. Patterns behave like gitignore globs anchored at the project root: each matches the named dir at any depth (e.g. `**/node_modules`), and matching a directory covers everything beneath it via the `ignore` crate's parent-match semantics.
- **Intended consumers** — the cleaning engine, which removes matched paths
  without asking for approval (see "Cleaning" above), and the discovery walk,
  which derives its descent-prune set from the same catalog (see "Discovery"
  below).
- **Observable hook** — `devclean safelist <path>` is the minimal diagnostic
  for the catalog.

A path is reported `safe` or `not-safe`, interpreted relative to the current
directory (treated as the project root); a path outside that root is an error
rather than a reported "not-safe". Directory-only patterns (a trailing `/`) are
matched against the path's actual kind on disk, so a plain file named `build`
is not reported safe by a `build/` pattern.

See `src/safelist.rs` for the implementation; `tests/safelist_cli.rs` for integration tests of the subcommand.

## Discovery

`devclean discovery` walks each configured `workspace_roots` entry up to
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
devclean discovery                        # uses workspace_roots from config
devclean --workspace ~/code discovery     # append a root from the CLI
```

With no workspace roots configured, discovery reports that and exits without
error. An unreadable or missing root is a hard error rather than a silent
empty result.

Each subcommand that runs discovery — `devclean discovery`, `devclean list`,
`devclean classification`, and the default run / `devclean clean` — renders a
live single-line progress indicator while walking each workspace root:
`walking: <path>` overwrites itself in place via a carriage return on a TTY,
so the display never scrolls. The same indicator continues through the later
phases: while each project's git state is examined the line reads
`classifying N/M: <path>`, while each cleanable project's reclaimable size is
computed the line reads `sizing N/M: <path>` (single-pass — bytes are stored
once and the aggregate is the sum of those already-computed bytes, not a
second walk), and while an approved project's untracked junk is deleted it
reads `cleaning N/M: <path>` (N is the 1-based project counter, M the total).
When stdout is piped or redirected, nothing is rendered — a stream of
CR-terminated partial paths would be garbage in a pipe or log file.
Terminal width is resolved via the `terminal_size` crate, falling back to
`COLUMNS` then a default of 80; paths that would wrap are truncated with a
leading ellipsis so the leaf (current directory) stays visible. Whenever
regular output must interleave with the indicator (per-project rows,
warnings), the progress line is first cleared in place; when a phase
completes, it is cleared and a newline is emitted so the following summary
line starts on a fresh line.

See `src/discovery.rs` for the matching and nesting rules;
`src/progress.rs` for the live indicator; `tests/discovery_cli.rs` for
integration tests of the subcommand; `tests/progress_cli.rs` for
integration tests of the classification/cleaning/sizing progress phases.

## Classification

`devclean classification` runs discovery, then classifies each project by
its git state and prints the results sorted by severity (most-needs-attention
first). Only status 5 is cleanable; statuses 1-4 each mean cleaning must wait.

| # | Status | Meaning |
|---|-------|---------|
| 1 | `no-git` | Has a project marker but is NOT git-initialized (or `.git` is a dangling gitdir pointer to a path that no longer exists). |
| 2 | `no-remote` | Git repo with no remote configured. |
| 3 | `unpushed` | Git repo with a remote but unpushed commits (or no upstream). |
| 4 | `wip` | Git repo with uncommitted work-in-progress (modified/staged tracked changes). |
| 5 | `cleanable` | Committed + pushed, AND has untracked junk that is NOT devcleanignored. |
| 6 | `clean` | Committed + pushed with no untracked non-devcleanignored junk. Not a dirty status; sorts last. |

Precedence is the lowest-numbered (most-severe) status. A repo with both WIP
(4) and untracked junk is status 4, not 5 — it must not be cleaned while it has
uncommitted work. The `.devcleanignore` matcher (see above) decides whether
untracked junk is protected, which separates `cleanable` from `clean`.

```
devclean classification                 # uses workspace_roots from config
devclean --workspace ~/code classification
```

See `src/classify.rs` for the git-plumbing logic and status precedence;
`tests/classification_cli.rs` for integration tests. The interactive
cleaning flow that acts on these statuses is `devclean clean` (see
"Cleaning" above).

## Configuration

devclean reads a TOML config file from the platform config dir
(`~/.config/devclean/config.toml` on Linux,
`~/Library/Application Support/devclean/config.toml` on macOS,
`%APPDATA%\devclean\config.toml` on Windows). A missing file at that default
location is not an error: built-in defaults are used. A path you pass explicitly
with `--config` must exist — devclean exits non-zero rather than silently
falling back to defaults. The one exception is `devclean init`, which creates
the file (see below).

First run? `devclean init <path>` writes a pre-populated config file under the
platform config dir with the given path as the active workspace root, each
other `Config` field documented with its default, and a comment explaining how
to add more workspace roots. `init` refuses to overwrite an existing file at
the target — it exits non-zero with the existing path, and does not read or
use that file. `devclean --config <other>` writes to `<other>` instead
(creating parent directories); the same refuse-to-overwrite rule applies to an
explicit `--config` target.

```toml
# ~/.config/devclean/config.toml
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
devclean [FLAGS] list              # read-only: each project's git status
devclean [FLAGS] config            # print the resolved config (preserved)
devclean [FLAGS] clean             # destructive: interactive flow, each prompt
devclean [FLAGS] discovery         # each project with markers
devclean [FLAGS] classification    # each project classified, sorted
devclean [FLAGS] ignore <path>     # see `.devcleanignore` above
devclean [FLAGS] safelist <path>   # see "Safe-to-delete catalog" above
devclean [FLAGS] init <path>       # create a pre-populated config file (see above)

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
devclean --config ./devclean.toml --workspace ~/code --force clean
devclean --workspace ~/code --dry-run classification
```
