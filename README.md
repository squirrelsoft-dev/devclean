# devclean

Development environment cleanup CLI (scaffold).

This is an initial, minimal Rust CLI skeleton. Product behavior (discovery,
classification, cleaning) is not yet implemented; the binary currently loads its
configuration and can print the resolved config via `devclean list`.

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
devclean ignore path/to/check   # prints "ignored" or "not-ignored"
```

It loads the global file plus every `.devcleanignore` under the current
directory and tests the given path (relative to the current directory).
Discovery and cleaning are separate, not-yet-implemented features.

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
safe_delete = ["node_modules", "target"]
max_depth = 4
project_markers = [".git", "package.json", "Cargo.toml", "go.mod", "pyproject.toml", "pom.xml", "build.gradle", "*.csproj"]
default_mode = "interactive"   # or "force"
```

Defaults:

- `workspace_roots` — `[]`
- `safe_delete` — `[]`
- `max_depth` — `4`
- `project_markers` — the list shown above
- `default_mode` — `interactive`

### CLI flags (override config)

Flags are top-level and must be given *before* the subcommand:

```sh
devclean [FLAGS] list            # print the resolved config

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
