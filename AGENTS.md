# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Stack

- Rust binary crate (`devclean`), edition 2024.
- CLI parsing via `clap` with the `derive` feature (`src/main.rs`). Subcommands are optional (`Option<Subcommand>`); running with no subcommand still prints the placeholder banner.
- Config is TOML loaded in `src/config.rs` (`serde` + `toml`), cross-platform config dir via the `dirs` crate. A missing file at the *default* path falls back to `Config::default()`, but an explicit `--config` path that does not exist is a hard error (enforced in `main::load_cli_config`, shared by every subcommand that loads config, not in `config`). CLI flags merge on top via `Config::apply_overrides`.

## Build / test

- Build, run, and test commands: see `README.md`.
- `Cargo.lock` is committed (binary crate); `.gitignore` deliberately omits it.

## Config surface

- User-facing config docs (file location, TOML keys, defaults, CLI flags) live in `README.md`; the resolution logic is `config::default_config_path`.
- Top-level flags are not `global`, so clap requires them *before* the subcommand (`devclean --force list`, never `devclean list --force`).
- `--force` and `--dry-run` conflict at the clap layer (`conflicts_with`), so no runtime precedence logic exists or should be added.
- `devclean list` is the observable config-printing command; `devclean discovery` finds projects (issue #5 — see `src/discovery.rs`); `devclean ignore` and `devclean safelist` are debug hooks for the matcher and the safe-to-delete catalog. All four are documented in `README.md`; classification/cleaning are separate issues.

## Safe-to-delete catalog

- The catalog reuses the `.devcleanignore` gitignore matcher rather than its own glob logic, so pattern semantics are the ignore matcher's semantics. Extend the catalog by editing `BUILT_IN_DEFAULTS` in `src/safelist.rs`, never by adding a second matching path.
- `safe_delete` in `Config` **extends** (never replaces) the built-in list. This is the contract users rely on; the regression test is `config_addition_is_recognized_as_safe` in `src/safelist.rs`.
- Owners: `README.md` for the user-facing catalog and the `devclean safelist` helper; `src/safelist.rs` module docs and rustdoc for matching semantics and constructor contracts. Unit tests live in `src/safelist.rs`, CLI-level tests in `tests/safelist_cli.rs`.

## Ignore matcher

- `.devcleanignore` is gitignore semantics, not a bespoke format: a global
  `~/.devcleanignore` plus per-folder files, deepest layer winning. Reach for
  gitignore behavior when in doubt rather than inventing devclean-specific rules.
- `is_ignored == true` means **protected**: cleaning must never act on that path.
  This is the matcher's whole contract with the not-yet-written cleaning engine.
- Owners: `README.md` for scopes, pattern syntax, precedence, and the
  `devclean ignore` helper; the `src/ignore.rs` module docs and rustdoc for the
  implementation sharp edges and constructor contracts.

## Classification

- `src/classify.rs` shells out to `git` plumbing (`git -C <project> ...`), consistent with the rest of the crate; it deliberately does NOT pull in `git2`. Status precedence is evaluated in order 1→5 (most-severe first); only status 5 (`Cleanable`) is cleanable. The `Clean` state is committed+pushed with NO untracked non-devcleanignored junk.
- Status 5 vs Clean uses `git ls-files --others` WITHOUT `--exclude-standard`, deliberately: build junk like `node_modules`/`target/` is gitignored by the *project*, but devclean exists to clean it, so gitignored files must stay visible. The devcleanignore matcher (issue #3 `is_ignored`) is the sole judge of whether untracked junk is protected. Owners: `README.md` classification table; `src/classify.rs` module docs; tests in `src/classify.rs` and `tests/classification_cli.rs`.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
