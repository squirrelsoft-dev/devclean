# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Stack

- Rust binary crate (`devclean`), edition 2024.
- CLI parsing via `clap` with the `derive` feature (`src/main.rs`). Subcommands are optional (`Option<Subcommand>`); running with no subcommand still prints the placeholder banner.
- Config is TOML loaded in `src/config.rs` (`serde` + `toml`), cross-platform config dir via the `dirs` crate. A missing file at the *default* path falls back to `Config::default()`, but an explicit `--config` path that does not exist is a hard error (enforced in `main::run_list`, not in `config`). CLI flags merge on top via `Config::apply_overrides`.

## Build / test

- Build, run, and test commands: see `README.md`.
- `Cargo.lock` is committed (binary crate); `.gitignore` deliberately omits it.

## Config surface

- User-facing config docs (file location, TOML keys, defaults, CLI flags) live in `README.md`; the resolution logic is `config::default_config_path`.
- Top-level flags are not `global`, so clap requires them *before* the subcommand (`devclean --force list`, never `devclean list --force`).
- `--force` and `--dry-run` conflict at the clap layer (`conflicts_with`), so no runtime precedence logic exists or should be added.
- `devclean list` is the only observable product command today; discovery/classification/cleaning are separate issues.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
