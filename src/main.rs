mod classify;
mod clean;
mod config;
mod discovery;
mod ignore;
mod interactive;
mod safelist;
mod output;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use config::{CliOverrides, Config};

/// devclean — development environment cleanup CLI.
///
/// Discovers projects under each configured workspace root, classifies each
/// project by its git state, and (by default) runs the interactive clean
/// flow on each cleanable project. `devclean list` prints each project's
/// status without cleaning; `devclean clean` runs the destructive interactive
/// flow; `devclean` (no subcommand) is the default run.
#[derive(Parser, Debug)]
#[command(name = "devclean", version, about)]
struct Cli {
    /// Override/append workspace roots (repeatable).
    #[arg(long, value_name = "PATH")]
    workspace: Vec<String>,

    /// Alternate config file path. Must exist — devclean exits non-zero if it does not.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Force mode: skip all prompts, auto-approve each surfaced item.
    /// Destructive: deletes on approval.
    #[arg(long, conflicts_with = "dry_run")]
    force: bool,

    /// Dry-run: show what would be deleted without deleting.
    /// Conflicts with --force at the clap layer — use one or the other.
    #[arg(long, conflicts_with = "force")]
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
    /// Debug helper: report whether a path is ignored by the loaded `.devcleanignore` rules.
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
    Clean,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        None => {
            // Default run: discover, classify, report each project sorted by
            // status, and run the interactive clean flow on each cleanable
            // project. Same code path as `devclean clean`.
            if let Err(e) = run_cleaning(&cli) {
                eprintln!("devclean: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::List) => {
            if let Err(e) = run_listing(&cli) {
                eprintln!("devclean: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Config) => {
            if let Err(e) = run_config(&cli) {
                eprintln!("devclean: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Ignore { path }) => {
            if let Err(e) = run_ignore(path) {
                eprintln!("devclean: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Safelist { path }) => {
            if let Err(e) = run_safelist(&cli, path) {
                eprintln!("devclean: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Discovery) => {
            if let Err(e) = run_discovery(&cli) {
                eprintln!("devclean: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Classification) => {
            if let Err(e) = run_classification(&cli) {
                eprintln!("devclean: {e}");
                std::process::exit(1);
            }
        }
        Some(Command::Clean) => {
            if let Err(e) = run_cleaning(&cli) {
                eprintln!("devclean: {e}");
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

/// `devclean config`: print the resolved configuration (workspace roots,
/// max_depth, default_mode, and per-invocation flags). Preserves the original
/// list-of-resolved-configuration behavior as a clearly-named alternative.
fn run_config(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (config_path, cfg) = load_cli_config(cli)?;
    let overrides = cli_overrides(cli);
    let cfg = cfg.apply_overrides(&overrides);

    println!("devclean resolved config:");
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

/// `devclean list`: show each discovered project with its git status, sorted
/// by severity (most-needs-attention first). Read-only — no cleaning.
///
/// Each row uses the formatted shape `[rank] path — label (reason)` with
/// color coding per status. Each cleanable row gets a trailing `(cleanable)`
/// indicator so the reader can tell which projects are subjects of the
/// interactive clean flow.
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
    let mut rows: Vec<(String, classify::Status)> = Vec::new();
    for d in &projects {
        let ignore_set = match ignore::IgnoreSet::load(&d.path) {
            Ok(set) => set,
            Err(e) => {
                eprintln!(
                    "warning: {}: skipped, could not load .devcleanignore: {e}",
                    d.path.display()
                );
                continue;
            }
        };
        let status = classify::classify(&d.path, &ignore_set);
        rows.push((d.path.display().to_string(), status));
    }
    rows.sort_by_key(|&(_, status)| status);

    // Gated on TTY — plain when piped, colored on a TTY. The color crate is
    // already in Cargo.toml; `output::color_runtime` delegates to it.
    println!(
        "{}",
        output::format_summary(rows.len(), None)
    );
    for (path, status) in &rows {
        println!(
            "{}",
            output::format_project_row(std::path::Path::new(path), *status, None)
        );
    }
    Ok(())
}

/// `devclean ignore <path>`: load the ignore set for the current directory and
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

/// `devclean safelist <path>`: load the default config (or `--config`), build
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

/// `devclean discovery`: walk each configured workspace root and print the
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

/// `devclean classification`: walk each configured workspace root, classify
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
    let mut rows: Vec<(String, classify::Status)> = Vec::new();
    for d in &projects {
        // Classification is a read-only survey over independent projects, so
        // one project with an unreadable `.devcleanignore` must not abort the
        // whole report. Skip just that project: falling back to an empty set
        // would treat nothing as protected and could report it `cleanable`,
        // which is the one verdict the cleaning engine acts destructively on.
        let ignore_set = match ignore::IgnoreSet::load(&d.path) {
            Ok(set) => set,
            Err(e) => {
                eprintln!(
                    "warning: {}: skipped, could not load .devcleanignore: {e}",
                    d.path.display()
                );
                continue;
            }
        };
        let status = classify::classify(&d.path, &ignore_set);
        rows.push((d.path.display().to_string(), status));
    }
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

/// `devclean clean`: the interactive cleaning flow (issue #8).
///
/// Sorts every discovered project by status, reports each non-cleanable one
/// with a one-line reason, and lists each cleanable one. For each cleanable
/// project, enumerates untracked items, prompts about each `Surfaced` item,
/// prompts about the project itself, then executes `git clean` if approved.
/// `--force` skips every prompt and auto-approves each surfaced item. `--dry-run`
/// shows what would be deleted, deletes nothing, skips prompts.
/// `devclean clean` (and the default run with no subcommand): the interactive
/// cleaning flow (issue #8). Sorts every discovered project by status, reports
/// each non-cleanable one with a one-line reason, and lists each cleanable one.
///
/// For each cleanable project, enumerates untracked items, prompts about each
/// `Surfaced` item, prompts about the project itself, then executes `git
/// clean` if approved. `--force` skips every prompt and auto-approves each
/// surfaced item; `--dry-run` shows what would be deleted, deletes nothing,
/// skips prompts.
///
/// Destructive: `devclean clean` deletes files on approval or under `--force`.
/// Only one subcommand runs the deletion; the default run runs it too.
fn run_cleaning(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (_config_path, cfg) = load_cli_config(cli)?;
    let cfg = cfg.apply_overrides(&cli_overrides(cli));

    let projects = discovery::discover(&cfg)?;
    if projects.is_empty() {
        println!("clean: no projects found");
        return Ok(());
    }

    // Classify every project, keeping each status and ignore-set for the
    // report and execution. Skip each project whose `.devcleanignore` could
    // not be loaded — same fail-safe as `run_classification`: one bad set
    // would otherwise mask a protected file as cleanable, and the engine
    // acts destructively on that verdict.
    let mut all_projects: Vec<(PathBuf, classify::Status, ignore::IgnoreSet)> = Vec::new();
    for d in &projects {
        let ignore_set = match ignore::IgnoreSet::load(&d.path) {
            Ok(set) => set,
            Err(e) => {
                eprintln!(
                    "warning: {}: skipped, could not load .devcleanignore: {e}",
                    d.path.display()
                );
                continue;
            }
        };
        let status = classify::classify(&d.path, &ignore_set);
        all_projects.push((d.path.clone(), status, ignore_set));
    }
    all_projects.sort_by_key(|&(_, status, _)| status);

    // Report phase: each non-cleanable project gets a one-line reason; cleanable
    // projects are listed separately as the cleanup subjects. For each cleanable
    // project, keep its index into `all_projects` (for the ignore set) and the
    // safe set built here, so execution below does not rebuild or re-find either.
    // Each row uses the formatted shape with color gating on TTY.
    // The header is prefixed with "clean:" — this is the destructive run, so
    // the reader knows which flow is about to execute.
    println!(
        "clean: {} project(s) — sorted by status",
        all_projects.len()
    );
    let mut cleanable_items: Vec<(PathBuf, Vec<clean::CleanItem>)> = Vec::new();
    let mut cleanable_meta: Vec<(usize, safelist::SafeSet)> = Vec::new();
    for (idx, (path, status, ignore_set)) in all_projects.iter().enumerate() {
        match status {
            classify::Status::Cleanable => {
                let safe_set = match safelist::SafeSet::from_config(path, &cfg) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!(
                            "warning: {}: skipped, could not build safe-to-delete set: {e}",
                            path.display()
                        );
                        continue;
                    }
                };
                let items = match clean::dry_run(path, ignore_set, &safe_set) {
                    Ok(items) => items,
                    Err(e) => {
                        eprintln!("warning: {}: clean failed: {e}", path.display());
                        continue;
                    }
                };
                println!(
                    "{}",
                    output::format_project_row(path, *status, None)
                );
                cleanable_items.push((path.clone(), items));
                cleanable_meta.push((idx, safe_set));
            }
            _ => {
                println!(
                    "{}",
                    output::format_project_row(path, *status, None)
                );
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
    for (r, (idx, safe_set)) in results.iter().zip(&cleanable_meta) {
        let will_execute = r.project_approved && !cli.dry_run;
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
            match clean::clean(&r.path, ignore_set, safe_set, &approved, cli.force, false) {
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
