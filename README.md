# devclean

Development environment cleanup CLI (scaffold).

The binary loads configuration, discovers projects, classifies them, and
cleanes them. The current surface covers discovery (`devclean discover`),
protection via `.devcleanignore` (`devclean ignore`), and the safe-to-delete
catalog (`devclean safelist`); classification and interactive flow are
separate issues.

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
Discovery and cleaning are separate, not-yet-implemented features.

## Safe-to-delete catalog

devclean keeps a catalog of paths that are safe to auto-delete — the ones a
cleaning run may remove without asking. `devclean safelist <path>` reports
whether a given path is in it. The catalog is:

- **Built-in defaults** — compiled into `src/safelist.rs` as `BUILT_IN_DEFAULTS` (a non-exhaustive list of common build/cache dirs/files: `node_modules`, `target`, `.next`, `.turbo`, `dist`, `build`, `__pycache__`, `.venv`, `venv`, `.pytest_cache`, `.mypy_cache`, `.gradle`, `bin/obj`, `out`, `coverage`, `.nuxt`, `.svelte-kit`, `.cache`, `.parcel-cache`).
- **Extension via `Config::safe_delete`** — user-supplied gitignore-style globs are appended (not replaced) to the built-in set. Patterns behave like gitignore globs anchored at the project root: each matches the named dir at any depth (e.g. `**/node_modules`), and matching a directory covers everything beneath it via the `ignore` crate's parent-match semantics.
- **Intended consumer** — the not-yet-implemented cleaning engine, which will remove matched paths without asking for approval. Nothing deletes anything today.
- **Observable hook** — `devclean safelist <path>` is the minimal diagnostic for the catalog; discovery and cleaning are separate, not-yet-implemented features.

A path is reported `safe` or `not-safe`, interpreted relative to the current
directory (treated as the project root); a path outside that root is an error
rather than a reported "not-safe". Directory-only patterns (a trailing `/`) are
matched against the path's actual kind on disk, so a plain file named `build`
is not reported safe by a `build/` pattern.

See `src/safelist.rs` for the implementation; `tests/safelist_cli.rs` for integration tests of the subcommand.

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

### Discovery

`devclean discover` walks each configured `workspace_root` up to `max_depth`
and reports every folder that contains one of the configured `project_markers`
(.git, package.json, Cargo.toml, go.mod, pyproject.toml, pom.xml,
build.gradle, *.csproj, or any user-supplied additions). Each discovered
path is absolute. See `src/discovery.rs` for the nesting rule (every marker
is a candidate project; nested projects within max_depth are all reported).

```
devclean discover              # prints detected projects for workspace roots
devclean discover --json       # machine-readable output
```

### CLI flags (override config)

Flags are top-level and must be given *before* the subcommand:

```sh
devclean [FLAGS] list            # print the resolved config
devclean [FLAGS] ignore <path>   # see `.devcleanignore` above
devclean [FLAGS] safelist <path> # see "Safe-to-delete catalog" above

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
