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
- `devclean list` is the observable config-printing command; `devclean discovery` finds projects (issue #5 — see `src/discovery.rs`); `devclean classification` classifies them (issue #6 — see `src/classify.rs`); `devclean ignore` and `devclean safelist` are debug hooks for the matcher and the safe-to-delete catalog; `devclean clean` is a non-destructive cleaning preview (issue #7 — see `src/clean.rs`). All six are documented in `README.md`; the interactive approval flow (#8) is a separate issue.

## Safe-to-delete catalog

- The catalog reuses the `.devcleanignore` gitignore matcher rather than its own glob logic, so pattern semantics are the ignore matcher's semantics. Extend the catalog by editing `BUILT_IN_DEFAULTS` in `src/safelist.rs`, never by adding a second matching path.
- `safe_delete` in `Config` **extends** (never replaces) the built-in list. This is the contract users rely on; the regression test is `config_addition_is_recognized_as_safe` in `src/safelist.rs`.
- Owners: `README.md` for the user-facing catalog and the `devclean safelist` helper; `src/safelist.rs` module docs and rustdoc for matching semantics and constructor contracts. Unit tests live in `src/safelist.rs`, CLI-level tests in `tests/safelist_cli.rs`.

## Ignore matcher

- `.devcleanignore` is gitignore semantics, not a bespoke format: a global
  `~/.devcleanignore` plus per-folder files, deepest layer winning. Reach for
  gitignore behavior when in doubt rather than inventing devclean-specific rules.
- `is_ignored == true` means **protected**: cleaning must never act on that path.
  This is the matcher's whole contract with the cleaning engine (`src/clean.rs`).
- Owners: `README.md` for scopes, pattern syntax, precedence, and the
  `devclean ignore` helper; the `src/ignore.rs` module docs and rustdoc for the
  implementation sharp edges and constructor contracts.

## Classification

- `src/classify.rs` shells out to `git` plumbing (`git -C <project> ...`), consistent with the rest of the crate; it deliberately does NOT pull in `git2`. Status precedence is evaluated in order 1→5 (most-severe first); only status 5 (`Cleanable`) is cleanable. The `Clean` state is committed+pushed with NO untracked non-devcleanignored junk.
- Status 5 vs Clean uses `git ls-files --others` WITHOUT `--exclude-standard`, deliberately: build junk like `node_modules`/`target/` is gitignored by the *project*, but devclean exists to clean it, so gitignored files must stay visible. The devcleanignore matcher (issue #3 `is_ignored`) is the sole judge of whether untracked junk is protected. Owners: `README.md` classification table; `src/classify.rs` module docs; tests in `src/classify.rs` and `tests/classification_cli.rs`.

## Cleaning

- `src/clean.rs` shells out to `git` plumbing (no `git2`), consistent with the rest of the crate. Enumerate untracked with `git ls-files --others --directory --no-empty-directory -z` **without** `--exclude-standard` (project-gitignored build junk must stay visible so devclean can clean it; `.devcleanignore` is the sole protection judge).
- Exclusions passed to `git clean -xfd -e` are one root-anchored, glob-escaped literal `/<rel_path>` per protected item plus per un-approved surfaced item — NOT the `.devcleanignore` source patterns verbatim. Re-using a nested layer's `/foo` pattern verbatim would exclude `<root>/foo` instead of `<root>/sub/foo`; anchoring each exclusion at the root is what makes nested layers correct. The original zshrc only read global+root ignore files so it never hit this.
- Granularity (from #6 review): a directory that is protected or safe is recorded once; a directory that is neither is re-listed at file granularity so content patterns (e.g. `*.js`) inside untracked directories still protect their files. `git ls-files --others -z -- <dir>` yields full repo-relative paths — use them directly, do not re-join onto `dir`.
- Deferred #3 findings resolved here: `is_ignored == false` for a `!`-whitelisted path means NOT protected (deletable subject to safe-list + approval); an absolute path on the cleaning path is classified `Protected` (fail-safe toward not-deleting, never fail-open) before the matcher is consulted.
- Public API seam for the interactive flow (#8): `clean(project, ignore_set, safe_set, approved, force, dry_run)` and `dry_run(...)`; `build_exclusions` is the exclusion-list builder. The `devclean clean` CLI hook is a **non-destructive preview** only — it never invokes `git clean` deletion, by design (#8 owns the interactive flow).
- Owners: `README.md` "Cleaning" section; `src/clean.rs` module docs and rustdoc; tests in `src/clean.rs`.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
