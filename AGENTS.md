# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Stack

- Rust binary crate (`devclean`), edition 2024.
- CLI parsing via `clap` with the `derive` feature (`src/main.rs`). Subcommands are optional (`Option<Subcommand>`); running with no subcommand runs the interactive clean flow for each cleanable project. Each subcommand is one of: `list` (read-only project listing, sorted by status), `config` (resolves config — preserves the original list-of-config behavior), `clean` (destructive interactive flow), `discovery`, `classification`, `ignore`, `safelist`, `init` (creates a pre-populated TOML config — issue #25, `src/main.rs::run_init`).
- Config is TOML loaded in `src/config.rs` (`serde` + `toml`), cross-platform config dir via the `dirs` crate. A missing file at the *default* path falls back to `Config::default()`, but an explicit `--config` path that does not exist is a hard error (enforced in `main::load_cli_config`, shared by every subcommand that loads config, not in `config`). CLI flags merge on top via `Config::apply_overrides`.
- Color crate: `owo-colors` in `Cargo.toml`, owned by `src/output.rs`. Gated on `std::io::IsTerminal`; plain text when piped, and `NO_COLOR` (any value) / `CLICOLOR=0` disable color even on a TTY. Legacy Windows conhost limitation (no VT enabling) is documented in `README.md`.

## Build / test

- Build, run, and test commands: see `README.md`.
- `Cargo.lock` is committed (binary crate); `.gitignore` deliberately omits it.

## Config surface

- User-facing config docs (file location, TOML keys, defaults, CLI flags) live in `README.md`; the resolution logic is `config::default_config_path`. Output formatting lives in `src/output.rs`.
- Top-level flags are not `global`, so clap requires them *before* the subcommand (`devclean --force list`, never `devclean list --force`).
- `--force` and `--dry-run` may be combined (the #8 flow removed the old clap `conflicts_with`; do not re-add it): `--force --dry-run` previews the force run. Precedence lives in one place each — `dry_run` alone gates execution, `force` alone gates prompting/auto-approval — do not add further runtime precedence logic. Regression test: `clean_force_dry_run_auto_approves_each_item` in `tests/clean_cli.rs`.
- `devclean list` is the read-only project listing command; `devclean config` is the resolved-config command (preserves the original list-of-config behavior). `devclean` (no subcommand) and `devclean clean` each run the full interactive clean flow — sorted report, each cleanable project enumerated and cleaned. `devclean discovery` finds projects (issue #5 — see `src/discovery.rs`); `devclean classification` classifies them (issue #6 — see `src/classify.rs`); `devclean ignore` and `devclean safelist` are debug hooks for the matcher and the safe-to-delete catalog; `devclean init <path>` creates a TOML config file pre-populated with each `Config` field's default and the given workspace root active (issue #25 — see `src/main.rs::run_init`); all eight subcommands are documented in `README.md`.

## Safe-to-delete catalog

- The catalog reuses the `.devcleanignore` gitignore matcher rather than its own glob logic, so pattern semantics are the ignore matcher's semantics. Extend the catalog by editing `BUILT_IN_DEFAULTS` in `src/safelist.rs`, never by adding a second matching path.
- `safe_delete` in `Config` **extends** (never replaces) the built-in list. This is the contract users rely on; the regression test is `config_addition_is_recognized_as_safe` in `src/safelist.rs`.
- The catalog is also discovery's single source of truth for artifact-directory descent pruning (`discovery::build_prune_set` — single-segment `**/<name>` or bare-name entries only); never add a second hardcoded artifact list. Semantics owner: `src/discovery.rs` module docs.
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

- `src/clean.rs` shells out to `git` plumbing (no `git2`), consistent with the rest of the crate. Enumerate untracked with `git ls-files --others --directory -z` **without** `--exclude-standard` (project-gitignored build junk must stay visible so devclean can clean it; `.devcleanignore` is the sole protection judge) and **without** `--no-empty-directory` (empty untracked dirs are deliberately enumerated so they are classified rather than silently deleted by `git clean -xfd`).
- Exclusions passed to `git clean -xfd -e` are one root-anchored, glob-escaped literal `/<rel_path>` per protected item plus per un-approved surfaced item — NOT the `.devcleanignore` source patterns verbatim. Re-using a nested layer's `/foo` pattern verbatim would exclude `<root>/foo` instead of `<root>/sub/foo`; anchoring each exclusion at the root is what makes nested layers correct. The original zshrc only read global+root ignore files so it never hit this.
- Granularity (from #6 review): a directory that is protected or safe is recorded once; a directory that is neither is re-listed at file granularity so content patterns (e.g. `*.js`) inside untracked directories still protect their files. `git ls-files --others -z -- <dir>` yields full repo-relative paths — use them directly, do not re-join onto `dir`. That re-listing yields files only, so it is paired with an on-disk walk (`discover_fileless_dirs`) that classifies nested file-less directories — otherwise `git clean -xfd` would silently delete them.
- Deferred #3 findings resolved here: `is_ignored == false` for a `!`-whitelisted path means NOT protected (deletable subject to safe-list + approval); an absolute path on the cleaning path is classified `Protected` (fail-safe toward not-deleting, never fail-open) before the matcher is consulted.
- Public API seam driven by the interactive flow (#8, `src/interactive.rs`): `clean(project, ignore_set, safe_set, approved, force, dry_run)` and `dry_run(...)`; `build_exclusions` is the exclusion-list builder. The `devclean clean` CLI hook is **destructive**: it deletes via `clean()` only for a project the user approved (or under `--force`), and never under `--dry-run`. Each subcommand — `devclean` (no subcommand, the default run) and `devclean clean` — runs the destructive interactive flow; `devclean list` only lists statuses.
- Owners: `README.md` "Cleaning" section; `src/clean.rs` module docs and rustdoc; unit tests in `src/clean.rs`, CLI-level tests in `tests/clean_cli.rs`.

## Live progress indicator

- `src/progress.rs` owns the live single-line progress writer: `ProgressWriter<W: Write>` is generic over any writer so tests can inject a `Vec<u8>` buffer. TTY-gated via `output::is_tty()`; each `update(path)` writes CR-prefixed `walking: <truncated path>` with no trailing newline; each `finish()` clears the line and emits a newline. Terminal width: `terminal_size` crate first, then `COLUMNS` env var, then default 80. Paths truncated with a leading ellipsis so the leaf stays visible; padded with spaces so each update fully overwrites the prior one.
- Integrated into `src/discovery.rs`: `discover()` constructs a `ProgressWriter::new(std::io::stdout())`, threads it through each `walk_root` call, and calls `finish()` once the walk ends — on success after all roots and on a `walk_root` error before returning it, so no partial progress line lingers ahead of the error. Every subcommand that runs discovery (`devclean discovery`, `devclean list`, `devclean classification`, the default run / `devclean clean`) gets the indicator automatically — no per-subcommand wiring.
- Unit tests in `src/progress.rs` inject a buffer instead of a real terminal — no real TTYs are spawned; see that module's `tests` for the covered cases (TTY gating, truncation incl. multibyte paths and tiny widths, overwrite, finish).
- Owners: `README.md` "Discovery" section; `src/progress.rs` module docs and rustdoc; the `walk_root` rustdoc in `src/discovery.rs`; unit tests in `src/progress.rs`.

## Disk savings (issue #18)

- `src/disk.rs` computes per-project reclaimable size (each cleanable project's `Safe` + `Surfaced` items summed via `WalkDir`, `symlink_metadata` for symlinks, tolerant of permission-denied paths and non-UTF-8 names).
- `src/output.rs` owns the display shape: `format_project_row` carries an optional size on each cleanable row (`path — cleanable (~2.3 GB)`); non-cleanable rows carry no size. `format_summary` carries an optional cleanable-count and an optional aggregate size (`listing: N project(s), M cleanable, ~X reclaimable`).
- `--dry-run` shows the same sizes without deleting anything; the computation reuses `clean::dry_run`.
- Sizes are human-readable (KB / MB / GB, 1024-based, ≥ 1.0 picks each unit) and approximate (the `~` form). The walk does not follow symlinks; each symlink counts its own size only. Permission-denied paths contribute 0, not an abort.
- Tests: unit tests in `src/disk.rs` and `src/output.rs`; integration tests in `tests/clean_cli.rs` verify the rendered output.
- Owners: `README.md` "Disk savings" section; `src/disk.rs` module docs; `src/output.rs` formatting contract; `src/main.rs` `run_listing` / `run_cleaning` callers.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
