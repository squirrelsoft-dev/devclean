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
