# devclean

Development environment cleanup CLI (scaffold).

The binary loads configuration, discovers projects, classifies their git
state, and cleans the cleanable ones. The current surface covers discovery
(`devclean discovery`), classification (`devclean classification`),
protection via `.devcleanignore` (`devclean ignore`), the safe-to-delete
catalog (`devclean safelist`), and the interactive cleaning flow
(`devclean clean`), which **deletes files** once approved (or with
`--force`); use `--dry-run` for a non-destructive preview.

## Build

```sh
cargo build
```

## Run

```sh
cargo run -- --version
cargo run -- list
```

## Test

```sh
cargo test
```

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
- `--dry-run` — non-destructive preview: print what would be deleted and
  delete nothing. Safe items are shown as `would delete`; surfaced items as
  `would prompt` (a real interactive run asks about them).
- `--force --dry-run` — preview the force run: every non-protected item is
  shown as `would delete`, nothing is deleted.

```sh
$ devclean clean                    # interactive: report, approve, then delete
$ devclean --dry-run clean          # preview only; deletes nothing
$ devclean --force clean            # DESTRUCTIVE: clean everything without prompting
$ devclean --force --dry-run clean  # preview what --force would delete
```

See `src/clean.rs` for the deletion engine and `src/interactive.rs` for the
approval state machine (the `clean` / `dry_run` / `build_exclusions` API is
the seam the interactive flow drives).

## Safe-to-delete catalog

devclean keeps a catalog of paths that are safe to auto-delete — the ones a
cleaning run may remove without asking. `devclean safelist <path>` reports
whether a given path is in it. The catalog is:

- **Built-in defaults** — compiled into `src/safelist.rs` as `BUILT_IN_DEFAULTS` (a non-exhaustive list of common build/cache dirs/files: `node_modules`, `target`, `.next`, `.turbo`, `dist`, `build`, `__pycache__`, `.venv`, `venv`, `.pytest_cache`, `.mypy_cache`, `.gradle`, `bin/obj`, `out`, `coverage`, `.nuxt`, `.svelte-kit`, `.cache`, `.parcel-cache`).
- **Extension via `Config::safe_delete`** — user-supplied gitignore-style globs are appended (not replaced) to the built-in set. Patterns behave like gitignore globs anchored at the project root: each matches the named dir at any depth (e.g. `**/node_modules`), and matching a directory covers everything beneath it via the `ignore` crate's parent-match semantics.
- **Intended consumer** — the cleaning engine, which removes matched paths
  without asking for approval. See "Cleaning" above.
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
a direct child, and so on. The walk never rises above a root, and symlinks are
not followed. Only `.git` boundaries define standalone projects: a nested
`.git` is reported as a separate project, but a non-git marker (`Cargo.toml`,
`package.json`, ...) in a subfolder of an ancestor git worktree is part of that
project, not a new one — a monorepo with a root `.git` and marker-bearing
package folders yields one entry. A non-git marker at a workspace root is
always reported; a git repo above the root is never consulted. Reported paths
are printed as they were walked from the workspace root, so configuring
absolute roots (the usual case) yields absolute output.

```
devclean discovery                        # uses workspace_roots from config
devclean --workspace ~/code discovery     # append a root from the CLI
```

With no workspace roots configured, discovery reports that and exits without
error. An unreadable or missing root is a hard error rather than a silent
empty result.

See `src/discovery.rs` for the matching and nesting rules;
`tests/discovery_cli.rs` for integration tests of the subcommand.

## Classification

`devclean classification` runs discovery, then classifies each project by
its git state and prints the results sorted by severity (most-needs-attention
first). Only status 5 is cleanable; statuses 1-4 each mean cleaning must wait.

| # | Status | Meaning |
|---|-------|---------|
| 1 | `no-git` | Has a project marker but is NOT git-initialized. |
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
falling back to defaults.

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

Flags are top-level and must be given *before* the subcommand:

```sh
devclean [FLAGS] list            # print the resolved config
devclean [FLAGS] ignore <path>   # see `.devcleanignore` above
devclean [FLAGS] safelist <path> # see "Safe-to-delete catalog" above
devclean [FLAGS] discovery       # see "Discovery" above
devclean [FLAGS] classification  # see "Classification" above
devclean [FLAGS] clean           # see "Cleaning" above (interactive; deletes on approval)

  --workspace <path>             # append a workspace root (repeatable)
  --config <path>                # alternate config file (must exist)
  --force                        # force mode (overrides default_mode)
  --dry-run                      # show what would be deleted
  --verbose                      # verbose output
```

`--force` and `--dry-run` may be combined: `--force --dry-run` previews what
a force run would delete, without deleting anything (`--dry-run` always wins
on execution; `--force` only widens what is auto-approved for display).

For example:

```sh
devclean --config ./devclean.toml --workspace ~/code --force list
```
