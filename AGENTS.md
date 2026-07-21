# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Stack

- Rust binary crate (`offcut`), edition 2024.
- CLI parsing via `clap` with the `derive` feature (`src/main.rs`). Subcommands are optional (`Option<Subcommand>`); running with no subcommand runs the interactive clean flow for each cleanable project. Each subcommand is one of: `list` (read-only project listing, sorted by status), `config` (resolves config — preserves the original list-of-config behavior), `clean` (destructive interactive flow), `discovery`, `classification`, `ignore`, `safelist`, `init` (creates a pre-populated TOML config — issue #25, `src/main.rs::run_init`).
- Config is TOML loaded in `src/config.rs` (`serde` + `toml`), cross-platform config dir via the `dirs` crate. A missing file at the *default* path falls back to `Config::default()`, but an explicit `--config` path that does not exist is a hard error (enforced in `main::load_cli_config`, shared by every subcommand that loads config, not in `config`). CLI flags merge on top via `Config::apply_overrides`.
- Color crate: `owo-colors` in `Cargo.toml`, owned by `src/output.rs`. Gated on `std::io::IsTerminal`; plain text when piped, and `NO_COLOR` (any value) / `CLICOLOR=0` disable color even on a TTY. Legacy Windows conhost limitation (no VT enabling) is documented in `README.md`.

## Build / test

- Build, run, and test commands: see `README.md`.
- `Cargo.lock` is committed (binary crate); `.gitignore` deliberately omits it.

## Release

- `Cargo.toml` package `version` is the single release version source: the
  tag-triggered workflow in `.github/workflows/release.yml` hard-fails unless
  the pushed tag is exactly `v<Cargo.toml version>`. Never bump a version
  anywhere else.
- Release operation details (tagging flow, build targets, archive naming,
  checksums, macOS signing status): see `README.md` "Release".

## Config surface

- User-facing config docs (file location, TOML keys, defaults, CLI flags) live in `README.md`; the resolution logic is `config::default_config_path`. Output formatting lives in `src/output.rs`.
- Top-level flags are not `global`, so clap requires them *before* the subcommand (`offcut --force list`, never `offcut list --force`).
- `--force` and `--dry-run` may be combined (the #8 flow removed the old clap `conflicts_with`; do not re-add it): `--force --dry-run` previews the force run. Precedence lives in one place each — `dry_run` alone gates execution, `force` alone gates prompting/auto-approval — do not add further runtime precedence logic. Regression test: `clean_force_dry_run_auto_approves_each_item` in `tests/clean_cli.rs`.
- `offcut list` is the read-only project listing command; `offcut config` is the resolved-config command (preserves the original list-of-config behavior). `offcut` (no subcommand) and `offcut clean` each run the full interactive clean flow — sorted report, each cleanable project enumerated and cleaned. `offcut clean <PROJECT_PATH>` scopes discovery and deletion strictly to the named project via `discovery::discover_single` (bypasses the workspace-root walk entirely; no neighboring project is discovered or cleaned). The path is canonicalized and must be the git project root: a non-directory, missing, or inside-a-git-worktree path is a hard error raised before any classification or deletion (`discovery::enclosing_git_root`, via `git rev-parse --show-toplevel`), so the bypass cannot fail open on a subdirectory the walk would never report. A nested repo (submodule/vendored clone) is its own root and may be targeted; a non-git directory is still accepted and reported as `no-git`. The caller's shell expands `~` — offcut does not do tilde expansion. Every top-level flag still applies; `--workspace` is accepted but ignored in this mode, and combining it with a `PROJECT_PATH` emits one concise stderr notice that the path scopes the run and `--workspace` is ignored (config-file `workspace_roots` are not flagged — they are a standing setting, not a per-invocation mistake). Regression tests: `clean_project_path_*` in `tests/clean_cli.rs`, `discover_single_*` and `normalized_absolute_*` (the canonicalize-failure fallback, which must stay shape-equal to `enclosing_git_root`'s output or a real root would be false-rejected) in `src/discovery.rs`. `offcut discovery` finds projects (issue #5 — see `src/discovery.rs`); `offcut classification` classifies them (issue #6 — see `src/classify.rs`); `offcut ignore` and `offcut safelist` are debug hooks for the matcher and the safe-to-delete catalog; `offcut init <path>` creates a TOML config file pre-populated with each `Config` field's default and the given workspace root active (issue #25 — see `src/main.rs::run_init`); all eight subcommands are documented in `README.md`.

## Safe-to-delete catalog

- The catalog reuses the `.offcutignore` gitignore matcher rather than its own glob logic, so pattern semantics are the ignore matcher's semantics. Extend the catalog by editing `BUILT_IN_DEFAULTS` in `src/safelist.rs`, never by adding a second matching path.
- `safe_delete` in `Config` **extends** (never replaces) the built-in list. This is the contract users rely on; the regression test is `config_addition_is_recognized_as_safe` in `src/safelist.rs`.
- The catalog is also discovery's single source of truth for artifact-directory descent pruning (`discovery::build_prune_set` — single-segment `**/<name>` or bare-name entries only); never add a second hardcoded artifact list. Semantics owner: `src/discovery.rs` module docs.
- Owners: `README.md` for the user-facing catalog and the `offcut safelist` helper; `src/safelist.rs` module docs and rustdoc for matching semantics and constructor contracts. Unit tests live in `src/safelist.rs`, CLI-level tests in `tests/safelist_cli.rs`.

## Ignore matcher

- `.offcutignore` is gitignore semantics, not a bespoke format: a global
  `~/.offcutignore` plus per-folder files, deepest layer winning. Reach for
  gitignore behavior when in doubt rather than inventing offcut-specific rules.
- `is_ignored == true` means **protected**: cleaning must never act on that path.
  This is the matcher's whole contract with the cleaning engine (`src/clean.rs`).
- Owners: `README.md` for scopes, pattern syntax, precedence, and the
  `offcut ignore` helper; the `src/ignore.rs` module docs and rustdoc for the
  implementation sharp edges and constructor contracts.

## Classification

- `src/classify.rs` shells out to `git` plumbing (`git -C <project> ...`), consistent with the rest of the crate; it deliberately does NOT pull in `git2`. Status precedence is evaluated in order 1→5 (most-severe first); only status 5 (`Cleanable`) is cleanable. The `Clean` state is committed+pushed with NO untracked non-offcutignored junk.
- Status 5 vs Clean uses `git ls-files --others` WITHOUT `--exclude-standard`, deliberately: build junk like `node_modules`/`target/` is gitignored by the *project*, but offcut exists to clean it, so gitignored files must stay visible. The offcutignore matcher (issue #3 `is_ignored`) is the sole judge of whether untracked junk is protected. Owners: `README.md` classification table; `src/classify.rs` module docs; tests in `src/classify.rs` and `tests/classification_cli.rs`.

## Cleaning

- `src/clean.rs` shells out to `git` plumbing (no `git2`), consistent with the rest of the crate. Enumerate untracked with `git ls-files --others --directory -z` **without** `--exclude-standard` (project-gitignored build junk must stay visible so offcut can clean it; `.offcutignore` is the sole protection judge) and **without** `--no-empty-directory` (empty untracked dirs are deliberately enumerated so they are classified rather than silently deleted by `git clean -xfd`).
- Exclusions passed to `git clean -xfd -e` are one root-anchored, glob-escaped literal `/<rel_path>` per protected item plus per un-approved surfaced item — NOT the `.offcutignore` source patterns verbatim. Re-using a nested layer's `/foo` pattern verbatim would exclude `<root>/foo` instead of `<root>/sub/foo`; anchoring each exclusion at the root is what makes nested layers correct. The original zshrc only read global+root ignore files so it never hit this.
- Granularity (from #6 review): a directory that is protected or safe is recorded once; a directory that is neither is re-listed at file granularity so content patterns (e.g. `*.js`) inside untracked directories still protect their files. `git ls-files --others -z -- <dir>` yields full repo-relative paths — use them directly, do not re-join onto `dir`. That re-listing yields files only, so it is paired with an on-disk walk (`discover_fileless_dirs`) that classifies nested file-less directories — otherwise `git clean -xfd` would silently delete them.
- Deferred #3 findings resolved here: `is_ignored == false` for a `!`-whitelisted path means NOT protected (deletable subject to safe-list + approval); an absolute path on the cleaning path is classified `Protected` (fail-safe toward not-deleting, never fail-open) before the matcher is consulted.
- Public API seam driven by the interactive flow (#8, `src/interactive.rs`): `clean(project, ignore_set, safe_set, approved, force, dry_run)` and `dry_run(...)`; `build_exclusions` is the exclusion-list builder. The `offcut clean` CLI hook is **destructive**: it deletes via `clean()` only for a project the user approved (or under `--force`), and never under `--dry-run`. Each subcommand — `offcut` (no subcommand, the default run) and `offcut clean` — runs the destructive interactive flow; `offcut list` only lists statuses.
- Owners: `README.md` "Cleaning" section; `src/clean.rs` module docs and rustdoc; unit tests in `src/clean.rs`, CLI-level tests in `tests/clean_cli.rs`.

## Live progress indicator

- `src/progress.rs` owns the live single-line progress writer: `ProgressWriter<W: Write>` is generic over any writer so tests can inject a `Vec<u8>` buffer. TTY-gated via `output::is_tty()`; each `update(path)` writes CR-prefixed `walking: <truncated path>` with no trailing newline; `update_phase(phase, idx, total, path)` writes `<phase> N/M: <truncated path>` the same way (`classifying`/`sizing`/`cleaning` counted labels); `clear()` erases the line in place with no newline — call it before any `println!`/`eprintln!` while a line is live, or the padded line wraps and lingers on screen; `finish()` clears and emits a newline. Terminal width: `terminal_size` crate first, then `COLUMNS` env var, then default 80. Paths truncated with a leading ellipsis so the leaf stays visible; padded with spaces so each update fully overwrites the prior one.
- Integrated into `src/discovery.rs`: `discover()` constructs a `ProgressWriter::new(std::io::stdout())`, threads it through each `walk_root` call, and calls `finish()` once the walk ends — on success after all roots and on a `walk_root` error before returning it, so no partial progress line lingers ahead of the error. Every subcommand that runs the walk (`offcut discovery`, `offcut list`, `offcut classification`, the default run / `offcut clean`) gets the indicator automatically — no per-subcommand wiring. `offcut clean <PROJECT_PATH>` goes through `discover_single`, which has no walk to report on, so it renders no `walking:` line; the counted phases below still render.
- Classification, sizing, and cleaning phases: `main.rs::classify_projects` is the single shared classify-with-progress loop (used by `run_listing`, `run_classification`, `run_cleaning`) rendering `classifying N/M`; do not re-inline per-caller copies. The sizing loops in `run_listing` and `run_cleaning` each render `sizing N/M` around each cleanable project's `dry_run` + `compute_reclaimable_size` (single-pass — see "Disk savings" below). The clean execution loop in `run_cleaning` renders `cleaning N/M` around the actual `clean::clean` call and `clear()`s before each interleaved per-project output block.
- Unit tests in `src/progress.rs` inject a buffer instead of a real terminal — no real TTYs are spawned; see that module's `tests` for the covered cases (TTY gating, truncation incl. multibyte paths and tiny widths, overwrite, clear, finish).
- Owners: `README.md` "Discovery" section; `src/progress.rs` module docs and rustdoc; the `walk_root` rustdoc in `src/discovery.rs`; unit tests in `src/progress.rs`, CLI-level tests in `tests/progress_cli.rs`.

## Disk savings (issue #18)

- `src/disk.rs` computes per-project reclaimable size (each cleanable project's `Safe` + `Surfaced` items summed via `WalkDir`; file sizes via `fs::metadata` — `stat`, O(1) per file, contents never read; `symlink_metadata` for symlinks; tolerant of permission-denied paths and non-UTF-8 names).
- `src/output.rs` owns the display shape: `format_project_row` carries an optional size on each cleanable row (`path — cleanable (~2.3 GB)`); non-cleanable rows carry no size. `format_summary` carries an optional cleanable-count and an optional aggregate size (`listing: N project(s), M cleanable, ~X reclaimable`).
- Single-pass sizing in `run_listing` and `run_cleaning`: per-project bytes are stored in `per_project_bytes: Vec<Option<u64>>` alongside `per_project_size: Vec<Option<String>>`; the aggregate `total_reclaimable` is the sum of those already-computed bytes, so `dry_run` and `compute_reclaimable_size` each run exactly once per cleanable project. No second walk for the total. `run_cleaning`'s sizing pass additionally stashes each cleanable project's items and safe set in index-keyed side vectors; the report loop `take()`s them instead of re-running `SafeSet::from_config` and `dry_run`, so a project that failed during sizing is skipped consistently in both passes.
- `--dry-run` shows the same sizes without deleting anything; the computation reuses `clean::dry_run`.
- Sizes are human-readable (KB / MB / GB, 1024-based, ≥ 1.0 picks each unit) and approximate (the `~` form). The walk does not follow symlinks; each symlink counts its own size only. Permission-denied paths contribute 0, not an abort.
- Tests: unit tests in `src/disk.rs` and `src/output.rs`; integration tests in `tests/clean_cli.rs` verify the rendered output.
- Owners: `README.md` "Disk savings" section; `src/disk.rs` module docs; `src/output.rs` formatting contract; `src/main.rs` `run_listing` / `run_cleaning` callers.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
