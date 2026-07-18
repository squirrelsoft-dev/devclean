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
(`~/.config/devclean` on Linux, `~/Library/Application Support/devclean` on
macOS, `%APPDATA%\devclean` on Windows). Anything that is not a readable regular
file at that path — a missing file, or a directory — is not an error: built-in
defaults are used.

```toml
# ~/.config/devclean
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
  --config <path>                # alternate config file
  --force                        # force mode (overrides default_mode)
  --dry-run                      # show what would be deleted
  --verbose                      # verbose output
```

For example:

```sh
devclean --config ./devclean.toml --workspace ~/code --force list
```
