mod classify;
mod clean;
mod config;
mod discovery;
mod disk;
mod ignore;
mod interactive;
mod output;
mod progress;
mod safelist;

use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use config::{CliOverrides, Config};

/// offcut — development environment cleanup CLI.
///
/// Discovers projects under each configured workspace root, classifies each
/// project by its git state, and (by default) runs the interactive clean
/// flow on each cleanable project. `offcut list` prints each project's
/// status without cleaning; `offcut clean` runs the destructive interactive
/// flow; `offcut` (no subcommand) is the default run.
#[derive(Parser, Debug)]
#[command(name = "offcut", version, about)]
struct Cli {
    /// Override/append workspace roots (repeatable).
    #[arg(long, value_name = "PATH")]
    workspace: Vec<String>,

    /// Alternate config file path. Must exist for all subcommands except
    /// `init`, which creates it — an explicit --config target that does not
    /// exist is a hard error for every subcommand other than init; init
    /// refuses to clobber an existing target.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Force mode: skip all prompts, auto-approve each surfaced item.
    /// Destructive: deletes on approval. Combined with --dry-run it
    /// previews the force run without deleting.
    #[arg(long)]
    force: bool,

    /// Dry-run: show what would be deleted without deleting.
    #[arg(long)]
    dry_run: bool,

    /// Verbose output.
    #[arg(long)]
    verbose: bool,

    /// Subcommand.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List each discovered project with its git status, sorted by severity.
    /// No cleaning — read-only listing.
    List,
    /// Print the resolved configuration (workspace roots, max_depth, etc.).
    /// Preserves the original list-of-resolved-configuration behavior.
    Config,
    /// Debug helper: report whether a path is ignored by the loaded `.offcutignore` rules.
    Ignore {
        /// Path to test, relative to the current directory.
        path: String,
    },
    /// Debug helper: report whether a path is safe to delete according to the
    /// loaded `safe_delete` catalog.
    Safelist {
        /// Path to test, relative to the current directory.
        path: String,
    },
    /// Discover projects under each configured workspace root.
    Discovery,
    /// Classify each discovered project by its git state and print the result.
    Classification,
    /// Interactive cleaning flow: report each project sorted by status,
    /// then clean each cleanable project. Destructive: deletes on approval
    /// or under --force.
    ///
    /// When an optional `<PROJECT_PATH>` is supplied, discovery and deletion
    /// are scoped strictly to that one project — no other project is
    /// discovered or cleaned, even if it sits inside a configured workspace
    /// root. The path may be absolute or relative to the current directory;
    /// the caller's shell expands `~` (offcut does not perform tilde
    /// expansion itself). It must be the project root: a path inside a git
    /// project is rejected before anything is deleted. Every top-level flag
    /// (`--force`, `--dry-run`, `--verbose`, `--config`) still applies to the
    /// targeted project.
    Clean {
        /// Optional project root to clean. When supplied, discovery and
        /// deletion are scoped strictly to that project — no neighboring
        /// project is discovered or cleaned. Absolute or relative; the
        /// caller's shell expands `~`. A subdirectory of a git project is
        /// rejected — pass the project root.
        project_path: Option<PathBuf>,
    },
    /// Create a config file pre-populated with a workspace root. Writes a
    /// TOML template under the platform config dir (or an explicit --config
    /// target), with every Config field documented and the given workspace
    /// root active. Idempotent: does not clobber an existing file.
    Init {
        /// Workspace path to populate as a workspace root.
        workspace: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        None => {
            // Default run: discover, classify, report each project sorted by
            // status, and run the interactive clean flow on each cleanable
            // project. Same code path as `offcut clean` (without a project
            // path — the default run never targets a single project).
            if let Err(e) = run_cleaning(&cli, None) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::List) => {
            if let Err(e) = run_listing(&cli) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Config) => {
            if let Err(e) = run_config(&cli) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Ignore { path }) => {
            if let Err(e) = run_ignore(path) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Safelist { path }) => {
            if let Err(e) = run_safelist(&cli, path) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Discovery) => {
            if let Err(e) = run_discovery(&cli) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Classification) => {
            if let Err(e) = run_classification(&cli) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Clean { project_path }) => {
            // When the caller explicitly combines `--workspace` with a
            // `PROJECT_PATH`, the explicit path alone drives the run and
            // `--workspace` is silently a no-op. Emit one concise stderr
            // notice so a mistyped invocation is not mistaken for a wider
            // run. (Config-file `workspace_roots` are not flagged here —
            // they are a standing setting, not a per-invocation mistake.)
            if project_path.is_some() && !cli.workspace.is_empty() {
                eprintln!(
                    "offcut: clean <PROJECT_PATH> scopes the run to that project; --workspace {}",
                    if cli.workspace.len() == 1 {
                        format!("{} is ignored", cli.workspace[0])
                    } else {
                        format!("({} paths) is ignored", cli.workspace.len())
                    }
                );
            }
            if let Err(e) = run_cleaning(&cli, project_path.as_deref()) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Init { workspace }) => {
            if let Err(e) = run_init(&cli, workspace) {
                eprintln!("offcut: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn cli_overrides(cli: &Cli) -> CliOverrides {
    CliOverrides {
        workspace: cli.workspace.clone(),
        force: cli.force,
        dry_run: cli.dry_run,
        verbose: cli.verbose,
    }
}

/// Resolve `p` to a path relative to `root`, rejecting anything that is not
/// answerable against a project-anchored rule set.
///
/// Rules are anchored at the project root, so a path outside it cannot be
/// answered. Reporting a negative result for one would be a false negative in
/// the exact place a false negative is most dangerous — the caller would read
/// it as "not protected" / "safe to delete".
fn project_relative<'a>(
    root: &std::path::Path,
    p: &'a std::path::Path,
) -> Result<&'a std::path::Path, Box<dyn std::error::Error>> {
    let rel = match p.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) if p.is_relative() => p,
        Err(_) => {
            return Err(format!(
                "path is outside the project root {}: {}",
                root.display(),
                p.display()
            )
            .into());
        }
    };
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("path escapes the project root: {}", p.display()).into());
    }
    Ok(rel)
}

/// Resolve and load the config for this invocation, returning the path it was
/// read from (if any) alongside the loaded config.
///
/// A path the user typed explicitly must exist: silently falling back to
/// defaults would turn a typo into a plausible-looking run against the wrong
/// settings. Only the default location is allowed to be absent.
fn load_cli_config(cli: &Cli) -> Result<(Option<PathBuf>, Config), Box<dyn std::error::Error>> {
    if let Some(path) = &cli.config
        && !path.is_file()
    {
        return Err(format!("config file not found: {}", path.display()).into());
    }

    let config_path = cli.config.clone().or_else(config::default_config_path);
    let cfg = match &config_path {
        Some(path) => Config::load_or_default(path)?,
        None => Config::default(),
    };
    Ok((config_path, cfg))
}

/// `offcut init <WORKSPACE_PATH>`: create a TOML config file pre-populated
/// with every Config field's default and the given workspace root active.
///
/// Creates the parent directory (the `offcut/` dir under the platform config
/// dir, or all parents of an explicit --config target). Does not clobber an
/// existing file: if one is already present at the target, prints its path and
/// exits non-zero so scripts can detect the no-op.
///
/// The template body is generated by building a `Config` from
/// `Config::default()` with the user's path as the sole `workspace_roots`
/// entry, serializing it via `toml::to_string_pretty`, and prepending a
/// comment block. The resulting file parses back to a valid `Config`
/// (round-trip).
fn run_init(cli: &Cli, workspace: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let target = match &cli.config {
        Some(p) => p.clone(),
        None => config::default_config_path().ok_or("could not determine platform config dir")?,
    };

    // Idempotency: refuse to clobber an existing dirent. symlink_metadata
    // does not follow symlinks, so a dangling symlink at the target also
    // counts as occupied — nothing is lost; the error names the target.
    if target.symlink_metadata().is_ok() {
        if target.is_file() {
            return Err(format!("config file already exists: {}", target.display()).into());
        }
        return Err(format!("target exists and is not a file: {}", target.display()).into());
    }

    // Create parent directories of the target.
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    // Resolve the workspace path: canonicalize if it exists on disk, else
    // keep the raw input string. Matches how the rest of the crate handles
    // paths (dirs for the config dir, std::fs::canonicalize for user paths).
    let resolved = if workspace.exists() {
        std::fs::canonicalize(workspace)?
            .to_string_lossy()
            .to_string()
    } else {
        workspace.to_string_lossy().to_string()
    };

    let template = generate_init_template(&resolved);
    fs::write(&target, &template)?;

    println!("created config file: {}", target.display());
    Ok(())
}

/// Template body: each Config field with its default, generated from
/// `Config::default()` via `toml::to_string_pretty` so the template stays in
/// sync with the struct. The comment block is prepended by the caller.
fn generate_init_template(workspace: &str) -> String {
    let cfg = config::Config {
        workspace_roots: vec![PathBuf::from(workspace)],
        ..Default::default()
    };
    let body = toml::to_string_pretty(&cfg)
        .unwrap_or_else(|e| panic!("Config::default() must serialize: {e}"));
    format!("{}\n{body}", TEMPLATE_HEADER)
}

const TEMPLATE_HEADER: &str = r#"# offcut config — each field is documented below.
#
# workspace_roots: paths offcut scans for projects.
# safe_delete: glob patterns extending the built-in safe-to-delete catalog.
# max_depth: recursion depth for project discovery (default 4).
# project_markers: marker files used to detect projects (the default list
#   is shown below; add your own markers alongside the built-ins).
# default_mode: "interactive" (default) or "force".
#
# To add more workspace roots, append them to the workspace_roots array below.
"#;

/// `offcut config`: print the resolved configuration (workspace roots,
/// max_depth, default_mode, and per-invocation flags). Preserves the original
/// list-of-resolved-configuration behavior as a clearly-named alternative.
fn run_config(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (config_path, cfg) = load_cli_config(cli)?;
    let overrides = cli_overrides(cli);
    let cfg = cfg.apply_overrides(&overrides);

    println!("offcut resolved config:");
    match &config_path {
        Some(path) => println!("  config file: {}", path.display()),
        None => println!("  config file: (none)"),
    }
    println!("  default_mode: {}", cfg.default_mode);
    println!("  max_depth: {}", cfg.max_depth);
    println!("  workspace_roots:");
    if cfg.workspace_roots.is_empty() {
        println!("    (none)");
    } else {
        for r in &cfg.workspace_roots {
            println!("    - {}", r.display());
        }
    }
    println!(
        "  flags: dry_run={}, force={}, verbose={}",
        overrides.dry_run, overrides.force, overrides.verbose
    );

    Ok(())
}

/// Classify each discovered project with a live `classifying N/M: <path>`
/// progress line, returning each project's path, status, and loaded ignore
/// set in discovery order.
///
/// Classification is a read-only survey over independent projects, so one
/// project with an unreadable `.offcutignore` must not abort the whole
/// report. Skip just that project: falling back to an empty set would treat
/// nothing as protected and could report it `cleanable`, which is the one
/// verdict the cleaning engine acts destructively on. The live progress line
/// is cleared before the skip warning so the warning starts on a clean line
/// on a TTY instead of appending to the un-terminated progress line.
fn classify_projects(
    projects: &[discovery::DiscoveredProject],
) -> Vec<(PathBuf, classify::Status, ignore::IgnoreSet)> {
    let mut out = Vec::new();
    let mut progress = progress::ProgressWriter::new(std::io::stdout());
    for (i, d) in projects.iter().enumerate() {
        let ignore_set = match ignore::IgnoreSet::load(&d.path) {
            Ok(set) => set,
            Err(e) => {
                progress.clear();
                eprintln!(
                    "warning: {}: skipped, could not load .offcutignore: {e}",
                    d.path.display()
                );
                continue;
            }
        };
        progress.update_phase("classifying", i + 1, projects.len(), &d.path);
        let status = classify::classify(&d.path, &ignore_set);
        out.push((d.path.clone(), status, ignore_set));
    }
    progress.finish();
    out
}

/// `offcut list`: show each discovered project with its git status, sorted
/// by severity (most-needs-attention first). Read-only — no cleaning.
///
/// Each row uses the formatted shape `[rank] path — label (reason)` with
/// color coding per status. Cleanable rows carry a bold-green label so the
/// reader can tell which projects are subjects of the interactive clean flow.
fn run_listing(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (_config_path, cfg) = load_cli_config(cli)?;
    let cfg = cfg.apply_overrides(&cli_overrides(cli));

    let projects = discovery::discover(&cfg)?;
    if projects.is_empty() {
        println!("listing: no projects found");
        return Ok(());
    }

    // Collect each project's status, then sort by severity. `Status` derives
    // `Ord` over variants declared most-severe-first, so it sorts directly.
    let mut rows: Vec<(String, classify::Status)> = classify_projects(&projects)
        .into_iter()
        .map(|(path, status, _)| (path.display().to_string(), status))
        .collect();
    rows.sort_by_key(|&(_, status)| status);

    // Compute reclaimable size per project (only for cleanable rows) and
    // an aggregate across all cleanable projects. A project whose safe set
    // could not be built contributes zero — the row still displays but the
    // aggregate skips that project.
    let mut per_project_size: Vec<Option<String>> = vec![None; rows.len()];
    let mut per_project_bytes: Vec<Option<u64>> = vec![None; rows.len()];
    {
        let cleanable_indices: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|&(_, (_, s))| *s == classify::Status::Cleanable)
            .map(|(i, _)| i)
            .collect();
        let m = cleanable_indices.len();
        let mut progress = progress::ProgressWriter::new(std::io::stdout());
        for (i, &idx) in cleanable_indices.iter().enumerate() {
            let path = std::path::Path::new(&rows[idx].0);
            progress.update_phase("sizing", i + 1, m, path);
            let safe_set = match safelist::SafeSet::from_config(path, &cfg) {
                Ok(s) => s,
                Err(e) => {
                    progress.clear();
                    eprintln!(
                        "warning: {}: could not build safe-to-delete set: {e}",
                        rows[idx].0
                    );
                    continue;
                }
            };
            let ignore_set = match ignore::IgnoreSet::load(path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            match clean::dry_run(path, &ignore_set, &safe_set) {
                Ok(items) => match disk::compute_reclaimable_size(path, &items) {
                    Ok(bytes) => {
                        per_project_bytes[idx] = Some(bytes);
                        per_project_size[idx] = Some(disk::format_size(bytes));
                    }
                    Err(e) => {
                        progress.clear();
                        eprintln!(
                            "warning: {}: could not compute reclaimable size: {e}",
                            rows[idx].0
                        );
                    }
                },
                Err(e) => {
                    progress.clear();
                    eprintln!("warning: {}: dry_run failed: {e}", rows[idx].0);
                }
            }
        }
        progress.finish();
    }

    let cleanable_count = rows
        .iter()
        .filter(|&(_, s)| *s == classify::Status::Cleanable)
        .count();
    let total_reclaimable: u64 = per_project_bytes.iter().filter_map(|opt| *opt).sum();
    let total_reclaimable_str = if total_reclaimable > 0 {
        Some(disk::format_size(total_reclaimable))
    } else {
        None
    };

    // Gated on TTY — plain when piped, colored on a TTY. Passing `None`
    // lets `output::color` fall back to its runtime gate.
    println!(
        "{}",
        output::format_summary(
            rows.len(),
            None,
            if cleanable_count > 0 {
                Some(cleanable_count)
            } else {
                None
            },
            total_reclaimable_str.as_deref(),
        )
    );
    for (idx, (path, status)) in rows.iter().enumerate() {
        let size = per_project_size[idx].as_deref();
        println!(
            "{}",
            output::format_project_row(std::path::Path::new(path), *status, None, size,)
        );
    }
    Ok(())
}

/// `offcut ignore <path>`: load the ignore set for the current directory and
/// print whether `path` is ignored. Minimal observable hook for the ignore
/// matcher; cleaning is a separate issue.
fn run_ignore(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::current_dir()?;
    let set = ignore::IgnoreSet::load(&root)?;
    let p = std::path::Path::new(path);
    let rel = project_relative(&root, p)?;

    let ignored = set.is_ignored(rel);
    println!(
        "{}: {}",
        path,
        if ignored { "ignored" } else { "not-ignored" }
    );
    Ok(())
}

/// `offcut safelist <path>`: load the default config (or `--config`), build
/// the safe-to-delete set from built-ins plus the loaded `safe_delete`, and
/// report whether `path` is safe to delete. Minimal observable hook for the
/// catalog; cleaning is a separate issue.
fn run_safelist(cli: &Cli, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::current_dir()?;
    let p = std::path::Path::new(path);
    let rel = project_relative(&root, p)?;

    let (_config_path, cfg) = load_cli_config(cli)?;

    let set = safelist::SafeSet::from_config(&root, &cfg)?;
    let safe = set.is_safe(rel);

    println!("{}: {}", path, if safe { "safe" } else { "not-safe" });
    Ok(())
}

/// `offcut discovery`: walk each configured workspace root and print the
/// list of discovered projects (paths with markers). Classifying them is
/// `run_classification`; cleaning is a separate issue.
fn run_discovery(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (_config_path, cfg) = load_cli_config(cli)?;
    let cfg = cfg.apply_overrides(&cli_overrides(cli));

    let projects = discovery::discover(&cfg)?;
    if projects.is_empty() {
        println!("discovery: no projects found");
    } else {
        println!("discovered {} project(s):", projects.len());
        for p in &projects {
            println!("  {} ({})", p.path.display(), p.marker);
        }
    }
    Ok(())
}

/// `offcut classification`: walk each configured workspace root, classify
/// each discovered project by its git state, and print each one with its
/// status label, sorted by severity. Precedence is the lowest-numbered (most-
/// severe) status; status 5 is cleanable only. See issue #6 for the full spec.
fn run_classification(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (_config_path, cfg) = load_cli_config(cli)?;
    let cfg = cfg.apply_overrides(&cli_overrides(cli));

    let projects = discovery::discover(&cfg)?;
    if projects.is_empty() {
        println!("classification: no projects found");
        return Ok(());
    }

    // Collect each project's status, then sort by severity. `Status` derives
    // `Ord` over variants declared most-severe-first, so it sorts directly.
    let mut rows: Vec<(String, classify::Status)> = classify_projects(&projects)
        .into_iter()
        .map(|(path, status, _)| (path.display().to_string(), status))
        .collect();
    rows.sort_by_key(|&(_, status)| status);

    println!(
        "classification: {} project(s), sorted by severity",
        rows.len()
    );
    for (path, status) in &rows {
        println!("  [{}] {path} -> {}", status.rank(), status.label());
    }
    Ok(())
}

/// `offcut clean [PROJECT_PATH]` (and the default run with no subcommand): the
/// interactive cleaning flow (issue #8). Sorts every discovered project by
/// status, reports each non-cleanable one with a one-line reason, and lists
/// each cleanable one.
///
/// When `project_path` is `Some`, discovery is bypassed entirely and the
/// flow operates on exactly that one project: no other project is discovered
/// or cleaned, even if it lives inside a configured workspace root. The path
/// is resolved to an absolute directory (canonicalized when it exists); a
/// non-directory, missing, or non-root path (a directory inside a git
/// project) is a hard error, raised before any classification or deletion.
/// The caller's shell is expected to expand `~` — offcut does not perform
/// tilde expansion itself.
/// Every top-level flag (`--force`, `--dry-run`, `--verbose`, `--config`)
/// still applies to the targeted project.
///
/// For each cleanable project, enumerates untracked items, prompts about each
/// `Surfaced` item, prompts about the project itself, then executes `git
/// clean` if approved. `--force` skips every prompt and auto-approves each
/// surfaced item; `--dry-run` shows what would be deleted, deletes nothing,
/// skips prompts; `--force --dry-run` previews the force run.
///
/// Destructive: `offcut clean` deletes files on approval or under `--force`.
/// Only one subcommand runs the deletion; the default run runs it too.
fn run_cleaning(
    cli: &Cli,
    project_path: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (_config_path, cfg) = load_cli_config(cli)?;
    let cfg = cfg.apply_overrides(&cli_overrides(cli));

    let projects = match project_path {
        Some(p) => vec![discovery::discover_single(p, &cfg)?],
        None => discovery::discover(&cfg)?,
    };
    if projects.is_empty() {
        println!("clean: no projects found");
        return Ok(());
    }

    // Classify every project, keeping each status and ignore-set for the
    // report and execution. Skip each project whose `.offcutignore` could
    // not be loaded — see `classify_projects` for the fail-safe rationale:
    // one bad set would otherwise mask a protected file as cleanable, and
    // the engine acts destructively on that verdict.
    let mut all_projects: Vec<(PathBuf, classify::Status, ignore::IgnoreSet)> =
        classify_projects(&projects);
    all_projects.sort_by_key(|&(_, status, _)| status);

    // Report phase: each non-cleanable project gets a one-line reason; cleanable
    // projects are listed separately as the cleanup subjects. For each cleanable
    // project, keep its index into `all_projects` (for the ignore set) and the
    // safe set built here, so execution below does not rebuild or re-find either.
    // Each row uses the formatted shape with color gating on TTY.
    // The header is prefixed with "clean:" — this is the destructive run, so
    // the reader knows which flow is about to execute.
    let cleanable_count = all_projects
        .iter()
        .filter(|&(_, status, _)| *status == classify::Status::Cleanable)
        .count();
    // Per-project sizing in a single pass: raw bytes computed once per cleanable
    // project, summed into the aggregate total. Mirrors `run_listing`'s pattern.
    let mut per_project_size: Vec<Option<String>> = vec![None; all_projects.len()];
    let mut per_project_bytes: Vec<Option<u64>> = vec![None; all_projects.len()];
    let mut per_project_items: Vec<Option<Vec<clean::CleanItem>>> = vec![None; all_projects.len()];
    let mut per_project_safe_set: Vec<Option<safelist::SafeSet>> = vec![None; all_projects.len()];
    {
        let cleanable_indices: Vec<usize> = all_projects
            .iter()
            .enumerate()
            .filter(|&(_, (_, status, _))| *status == classify::Status::Cleanable)
            .map(|(i, _)| i)
            .collect();
        let m = cleanable_indices.len();
        let mut progress = progress::ProgressWriter::new(std::io::stdout());
        for (i, &idx) in cleanable_indices.iter().enumerate() {
            let path = &all_projects[idx].0;
            progress.update_phase("sizing", i + 1, m, path);
            let safe_set = match safelist::SafeSet::from_config(path, &cfg) {
                Ok(s) => s,
                Err(e) => {
                    progress.clear();
                    eprintln!(
                        "warning: {}: skipped, could not build safe-to-delete set: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let ignore_set = &all_projects[idx].2;
            match clean::dry_run(path, ignore_set, &safe_set) {
                Ok(items) => {
                    match disk::compute_reclaimable_size(path, &items) {
                        Ok(bytes) => {
                            per_project_bytes[idx] = Some(bytes);
                            per_project_size[idx] = Some(disk::format_size(bytes));
                        }
                        Err(e) => {
                            progress.clear();
                            eprintln!(
                                "warning: {}: could not compute reclaimable size: {e}",
                                path.display()
                            );
                        }
                    }
                    per_project_items[idx] = Some(items);
                    per_project_safe_set[idx] = Some(safe_set);
                }
                Err(e) => {
                    progress.clear();
                    eprintln!("warning: {}: dry_run failed: {e}", path.display());
                }
            }
        }
        progress.finish();
    }
    let total_reclaimable: u64 = per_project_bytes.iter().filter_map(|opt| *opt).sum();
    let total_reclaimable_str = if total_reclaimable > 0 {
        Some(disk::format_size(total_reclaimable))
    } else {
        None
    };
    // The aggregate reclaimable is printed alongside the header — the
    // per-project rows below each carry their own size too.
    println!(
        "clean: {} project(s) — sorted by status",
        all_projects.len()
    );
    println!(
        "{}",
        output::format_summary(
            all_projects.len(),
            None,
            if cleanable_count > 0 {
                Some(cleanable_count)
            } else {
                None
            },
            total_reclaimable_str.as_deref(),
        )
    );
    let mut cleanable_items: Vec<(PathBuf, Vec<clean::CleanItem>)> = Vec::new();
    let mut cleanable_meta: Vec<(usize, safelist::SafeSet)> = Vec::new();
    for (idx, (path, status, _ignore_set)) in all_projects.iter().enumerate() {
        match status {
            classify::Status::Cleanable => {
                let (items, safe_set) = match (
                    per_project_items[idx].take(),
                    per_project_safe_set[idx].take(),
                ) {
                    (Some(items), Some(safe_set)) => (items, safe_set),
                    _ => continue,
                };
                println!(
                    "{}",
                    output::format_project_row(
                        path,
                        *status,
                        None,
                        per_project_size[idx].as_deref()
                    )
                );
                cleanable_items.push((path.clone(), items));
                cleanable_meta.push((idx, safe_set));
            }
            _ => {
                println!("{}", output::format_project_row(path, *status, None, None));
            }
        }
    }

    // Zero cleanable: summary and exit 0 — no prompts, no enumeration,
    // nothing to clean. The flow only runs when there is a subject to clean.
    if cleanable_items.is_empty() {
        println!("clean: no cleanable projects — nothing to delete");
        return Ok(());
    }

    // Build the interactive inputs and run the state machine against the
    // process's stdin (locked, buffered for line reads). The flow itself
    // owns the decision state machine; the CLI hook owns the I/O plumbing.
    let inputs = interactive::InteractiveFlowInputs {
        all_projects: all_projects
            .iter()
            .map(|(p, s, _)| (p.clone(), *s))
            .collect(),
        per_project_items: cleanable_items,
        force: cli.force,
        dry_run: cli.dry_run,
    };
    let stdin_lock = std::io::stdin().lock();
    let mut reader = std::io::BufReader::new(stdin_lock);
    let results = interactive::run(inputs, &mut reader);

    // Outcome phase: `run` returns one result per cleanable project in the
    // same order as `cleanable_meta`. Print each project's outcome, then
    // execute the approved ones (execution is always suppressed by
    // --dry-run; --force approved everything without prompting).
    // The `cleaning N/M` progress line interleaves with per-project stdout
    // output, so it is cleared in place before each print — a println after
    // an un-cleared padded line would wrap and leave the progress line
    // permanently on screen instead of overwriting it.
    let mut clean_progress = progress::ProgressWriter::new(std::io::stdout());
    for (i, (r, (idx, safe_set))) in results.iter().zip(&cleanable_meta).enumerate() {
        let will_execute = r.project_approved && !cli.dry_run;
        clean_progress.update_phase("cleaning", i + 1, cleanable_meta.len(), &r.path);
        clean_progress.clear();
        println!(
            "clean {}: {} — {}",
            r.path.display(),
            if r.project_approved {
                "approved"
            } else {
                "skipped"
            },
            r.status.label()
        );
        for item in &r.items {
            let label = match item.classification {
                clean::Classification::Protected => "protected",
                clean::Classification::Safe => "safe-to-delete",
                clean::Classification::Surfaced => "surfaced",
            };
            let verdict = if r.would_delete.contains(&item.rel_path) {
                if will_execute {
                    " (deleting)"
                } else {
                    " (would delete)"
                }
            } else if cli.dry_run
                && !cli.force
                && item.classification == clean::Classification::Surfaced
            {
                // A real interactive run would ask about this item, so the
                // preview must not claim either fate.
                " (would prompt)"
            } else {
                " (kept)"
            };
            println!(
                "  {}{} [{}]{}",
                item.rel_path.display(),
                if item.is_dir { "/" } else { "" },
                label,
                verdict
            );
        }

        if will_execute {
            // `clean` builds the exclusion list from the approvals: Safe and
            // approved Surfaced items are left un-excluded so `git clean`
            // deletes them; Protected and unapproved Surfaced items are
            // excluded. The approvals are the Surfaced entries of the
            // would-delete list.
            let approved: Vec<PathBuf> = r
                .would_delete
                .iter()
                .filter(|p| {
                    r.items.iter().any(|i| {
                        i.rel_path == **p && i.classification == clean::Classification::Surfaced
                    })
                })
                .cloned()
                .collect();
            let ignore_set = &all_projects[*idx].2;
            clean_progress.update_phase("cleaning", i + 1, cleanable_meta.len(), &r.path);
            let outcome = clean::clean(&r.path, ignore_set, safe_set, &approved, cli.force, false);
            clean_progress.clear();
            match outcome {
                Ok(_) => {
                    println!(
                        "clean {}: deleted {} item(s)",
                        r.path.display(),
                        r.would_delete.len()
                    );
                }
                Err(e) => {
                    eprintln!("clean {}: failed: {e}", r.path.display());
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// init: unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod init_tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmpfile(contents: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        dir.push(format!(
            "offcut-init-test-{}-{}-{}.toml",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = fs::File::create(&dir).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        dir
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    /// Verify the generated template parses back to a Config whose
    /// workspace_roots contains the injected path and whose other fields all
    /// match Config::default(). This is the drift guard: if the struct changes
    /// and the template generator does not, the round-trip fails.
    #[test]
    fn template_round_trips_through_config_load() {
        let workspace = "/tmp/example-workspace";
        let text = generate_init_template(workspace);
        // The template must start with the comment block.
        assert!(text.starts_with('#'), "template missing comment block");
        // Parse via Config::load.
        let f = tmpfile(&text);
        let cfg = Config::load(&f).unwrap().unwrap();
        assert_eq!(cfg.workspace_roots, vec![PathBuf::from(workspace)]);
        assert_eq!(cfg.max_depth, 4);
        assert_eq!(cfg.default_mode, config::Mode::Interactive);
        assert!(cfg.safe_delete.is_empty());
        assert!(cfg.project_markers.contains(&".git".to_string()));
        cleanup(&f);
    }

    /// The generated body (everything after the comment block) must equal
    /// toml::to_string_pretty(Config::default()) with workspace_roots
    /// replaced by the injected path.
    #[test]
    fn template_body_matches_serialized_default() {
        let workspace = "/tmp/another-workspace";
        let text = generate_init_template(workspace);
        let default_cfg = config::Config {
            workspace_roots: vec![PathBuf::from(workspace)],
            ..Default::default()
        };
        let expected_body = toml::to_string_pretty(&default_cfg).unwrap();
        // Split off the comment block (everything up to and including the
        // first blank line after comments).
        let body = text.split_once("\n\n").map(|(_, b)| b).unwrap_or(&text);
        assert_eq!(body.trim(), expected_body.trim());
    }

    /// Idempotency: init refuses to clobber an existing file and returns an
    /// error carrying the existing path.
    #[test]
    fn init_refuses_to_clobber_existing_file() {
        let f = tmpfile("existing = true\n");
        let result = run_init(
            &Cli {
                config: Some(f.clone()),
                workspace: Vec::new(),
                force: false,
                dry_run: false,
                verbose: false,
                command: None,
            },
            &PathBuf::from("/tmp/work"),
        );
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("already exists"),
            "error message must name the existing file"
        );
        // File is untouched.
        assert!(f.is_file());
        cleanup(&f);
    }

    /// init creates the parent directory of the target when it does not exist.
    #[test]
    fn init_creates_parent_directory() {
        let mut dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        dir.push(format!(
            "offcut-init-parent-test-{}-{}",
            std::process::id(),
            n
        ));
        let target = dir.join("offcut").join("config.toml");
        // Ensure the parent does not exist.
        let _ = fs::remove_dir_all(&dir);
        let result = run_init(
            &Cli {
                config: Some(target.clone()),
                workspace: Vec::new(),
                force: false,
                dry_run: false,
                verbose: false,
                command: None,
            },
            &PathBuf::from("/tmp/work"),
        );
        assert!(result.is_ok());
        assert!(target.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    /// init injects the workspace path (resolved) into workspace_roots.
    #[test]
    fn init_injects_workspace_root() {
        let dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let target = dir.join(format!(
            "offcut-init-root-test-{}-{}",
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&target);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let result = run_init(
            &Cli {
                config: Some(target.clone()),
                workspace: Vec::new(),
                force: false,
                dry_run: false,
                verbose: false,
                command: None,
            },
            &PathBuf::from("/tmp/injected-workspace"),
        );
        assert!(result.is_ok());
        let cfg = Config::load(&target).unwrap().unwrap();
        assert!(
            cfg.workspace_roots
                .iter()
                .any(|p| p.to_string_lossy().contains("injected-workspace")),
            "workspace root not injected"
        );
        let _ = fs::remove_file(&target);
    }

    /// Explicit --config target that exists must not be clobbered either.
    #[test]
    fn explicit_config_target_respects_existing_file() {
        let mut dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        dir.push(format!(
            "offcut-init-explicit-test-{}-{}",
            std::process::id(),
            n
        ));
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("config.toml");
        fs::write(&existing, "existing = true\n").unwrap();
        let result = run_init(
            &Cli {
                config: Some(existing.clone()),
                workspace: Vec::new(),
                force: false,
                dry_run: false,
                verbose: false,
                command: None,
            },
            &PathBuf::from("/tmp/work"),
        );
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("already exists"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), "existing = true\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
