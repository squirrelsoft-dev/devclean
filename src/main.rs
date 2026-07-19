mod classify;
mod clean;
mod config;
mod discovery;
mod ignore;
mod interactive;
mod safelist;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use config::{CliOverrides, Config};

/// devclean - development environment cleanup CLI (scaffold)
#[derive(Parser, Debug)]
#[command(name = "devclean", version, about)]
struct Cli {
    /// Override/append workspace roots (repeatable).
    #[arg(long, value_name = "PATH")]
    workspace: Vec<String>,

    /// Alternate config file path.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Force mode: delete without prompting (overrides config `default_mode`).
    #[arg(long)]
    force: bool,

    /// Dry-run: show what would be deleted without deleting.
    #[arg(long)]
    dry_run: bool,

    /// Verbose output.
    #[arg(long)]
    verbose: bool,

    /// Subcommand. Omitting it prints the placeholder banner.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print the resolved configuration (workspace roots, max_depth, default_mode).
    List,
    /// Debug helper: report whether a path is ignored by the loaded `.devcleanignore` rules.
    ///
    /// Loads the global `~/.devcleanignore` plus every `.devcleanignore` under the
    /// current directory and prints `ignored` / `not-ignored` for the given path
    /// (interpreted relative to the current directory).
    Ignore {
        /// Path to test, relative to the current directory.
        path: String,
    },
    /// Debug helper: report whether a path is safe to delete according to the
    /// loaded `safe_delete` catalog (built-ins plus any `Config::safe_delete`
    /// additions).
    ///
    /// Loads the platform default config (respecting `--config`) and prints
    /// `safe` / `not-safe` for the given path, interpreted relative to the
    /// current directory. Cleaning is a separate issue.
    Safelist {
        /// Path to test, relative to the current directory.
        path: String,
    },
    /// Discover projects under each configured workspace root.
    ///
    /// Walks each workspace root up to `max_depth` and reports each folder
    /// that contains a marker from the resolved `project_markers` list. Each
    /// reported path is tagged with the marker that found it. Classifying those
    /// projects is `devclean classification`; cleaning is a separate issue.
    Discovery,
    /// Classify each discovered project by its git state and print the result
    /// sorted by status. Status 5 is the only cleanable state; status 1..4
    /// each signal that cleaning must wait. See issue #6 for the full spec.
    Classification,
    /// Interactive cleaning flow: report every project sorted by status,
    /// then for each cleanable project show what would be deleted and
    /// collect approval before running `git clean`. `--force` skips the
    /// prompts and auto-approves; `--dry-run` previews and deletes nothing.
    /// See issues #7 (engine) and #8 (flow) for the full spec.
    Clean,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        None => {
            println!("devclean - development environment cleanup CLI (scaffold)");
        }
        Some(Command::List) => {
            if let Err(e) = run_list(&cli) {
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
            if let Err(e) = run_clean(&cli) {
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

fn run_list(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
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
fn run_clean(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
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

    // Report phase: each non-cleanable project gets a one-line reason;
    // cleanable projects are listed separately as the cleanup subjects.
    // For each cleanable project, keep its index into `all_projects` (for
    // the ignore set) and the safe set built here, so execution below does
    // not rebuild or re-find either.
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
                    "  {} — {} (cleanable, status 5)",
                    path.display(),
                    status.label()
                );
                cleanable_items.push((path.clone(), items));
                cleanable_meta.push((idx, safe_set));
            }
            _ => {
                println!(
                    "  {} — {} (needs attention: {})",
                    path.display(),
                    status.label(),
                    interactive::status_reason(*status)
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
