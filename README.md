# devclean

Development environment cleanup CLI (scaffold).

The binary loads configuration and discovers projects. The current surface
covers discovery (`devclean discovery`), protection via `.devcleanignore`
(`devclean ignore`), and the safe-to-delete catalog (`devclean safelist`);
classification, cleaning, and the interactive flow are separate,
not-yet-implemented features.

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
`.devcleanignore` wins, layered on top of the global file. A path matched by an
ignore rule (including a `!` whitelist) is **protected** — cleaning never
removes it.

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
Cleaning is a separate, not-yet-implemented feature.

## Safe-to-delete catalog

devclean keeps a catalog of paths that are safe to auto-delete — the ones a
cleaning run may remove without asking. `devclean safelist <path>` reports
whether a given path is in it. The catalog is:

- **Built-in defaults** — compiled into `src/safelist.rs` as `BUILT_IN_DEFAULTS` (a non-exhaustive list of common build/cache dirs/files: `node_modules`, `target`, `.next`, `.turbo`, `dist`, `build`, `__pycache__`, `.venv`, `venv`, `.pytest_cache`, `.mypy_cache`, `.gradle`, `bin/obj`, `out`, `coverage`, `.nuxt`, `.svelte-kit`, `.cache`, `.parcel-cache`).
- **Extension via `Config::safe_delete`** — user-supplied gitignore-style globs are appended (not replaced) to the built-in set. Patterns behave like gitignore globs anchored at the project root: each matches the named dir at any depth (e.g. `**/node_modules`), and matching a directory covers everything beneath it via the `ignore` crate's parent-match semantics.
- **Intended consumer** — the not-yet-implemented cleaning engine, which will remove matched paths without asking for approval. Nothing deletes anything today.
- **Observable hook** — `devclean safelist <path>` is the minimal diagnostic for the catalog; cleaning is a separate, not-yet-implemented feature.

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
not followed. Nested projects are each reported separately — a repo with `.git`
that contains a subfolder with its own `Cargo.toml` yields two entries, with no
double-counting of either folder. Reported paths are printed as they were
walked from the workspace root, so configuring absolute roots (the usual case)
yields absolute output.

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
| - | `clean` | Committed + pushed with no untracked non-devcleanignored junk. |

Precedence is the lowest-numbered (most-severe) status. A repo with both WIP
(4) and untracked junk is status 4, not 5 — it must not be cleaned while it has
uncommitted work. The `.devcleanignore` matcher (see above) decides whether
untracked junk is protected, which separates `cleanable` from `clean`.

```
devclean classification                 # uses workspace_roots from config
devclean --workspace ~/code classification
```

See `src/classify.rs` for the git-plumbing logic and status precedence;
`tests/classification_cli.rs` for integration tests. Cleaning (#7) and the
interactive flow (#8) are separate issues.

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

  --workspace <path>             # append a workspace root (repeatable)
  --config <path>                # alternate config file (must exist)
  --force                        # force mode (overrides default_mode)
  --dry-run                      # show what would be deleted
  --verbose                      # verbose output
```

`--force` and `--dry-run` are mutually exclusive; passing both is an error.

For example:

```sh
devclean --config ./devclean.toml --workspace ~/code --force list
```
