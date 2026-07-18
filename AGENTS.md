# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Stack

- Rust binary crate (`devclean`), edition 2024.
- CLI parsing via `clap` with the `derive` feature (`src/main.rs`). Subcommands are optional (`Option<Subcommand>`); running with no subcommand still prints the placeholder banner.
- Config is TOML loaded in `src/config.rs` (`serde` + `toml`), cross-platform config dir via the `dirs` crate. Missing config file falls back to `Config::default()`; CLI flags merge on top via `Config::apply_overrides`.

## Build / test

- Build, run, and test commands: see `README.md`.
- `Cargo.lock` is committed (binary crate); `.gitignore` deliberately omits it.

## Config surface

- Config file location: platform config dir + `devclean` (see `config::default_config_path`).
- `devclean list` prints the resolved config (the only observable product command today); discovery/classification/cleaning are separate issues and not implemented here.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
