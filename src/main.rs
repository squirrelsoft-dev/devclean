mod classify;
mod clean;
mod config;
mod discovery;
mod disk;
mod ignore;
mod interactive;
mod json;
mod output;
mod progress;
mod safelist;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

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
    #[arg(long, value_name = "PATH", global = true)]
    workspace: Vec<String>,

    /// Alternate config file path. Must exist for all subcommands except
    /// `init`, which creates it — an explicit --config target that does not
    /// exist is a hard error for every subcommand other than init; init
    /// refuses to clobber an existing target.
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,

    /// Force mode: skip all prompts, auto-approve each surfaced item.
    /// Destructive: deletes on approval. Combined with --dry-run it
    /// previews the force run without deleting.
    ///
    /// Accepted before or after the subcommand (it is a global flag).
    #[arg(long, global = true)]
    force: bool,

    /// Dry-run: show what would be deleted without deleting.
    ///
    /// Accepted before or after the subcommand (it is a global flag).
    #[arg(long, global = true)]
    dry_run: bool,

    /// Verbose output.
    #[arg(long, global = true)]
    verbose: bool,

    /// Emit one machine-readable JSON document on stdout instead of the
    /// human-readable rendering. No ANSI sequences, progress rendering,
    /// prompts, or extra text on stdout; diagnostics stay on stderr. See
    /// `docs/json-output.md` for the schema and exit behavior.
    ///
    /// Accepted before or after the subcommand (it is a global flag).
    #[arg(long, global = true)]
    json: bool,

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
    /// (`--force`, `--dry-run`, `--config`) still applies to the targeted
    /// project; `--workspace` is accepted but ignored — the path
    /// alone scopes the run, and combining the two prints a notice saying so.
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
        #[arg(value_name = "WORKSPACE")]
        workspace_path: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    // Enable JSON mode once, before any run function touches stdout. The
    // gate suppresses the rich terminal UI, color, and live progress so
    // stdout carries exactly one JSON document.
    output::set_json_mode(cli.json);

    if cli.json {
        std::process::exit(run_json(&cli));
    }

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
        Some(Command::Init { workspace_path }) => {
            // `init <WORKSPACE>` uses the positional exclusively; a global
            // `--workspace` flag is silently a no-op. Emit one concise stderr
            // notice (the same treatment `clean <PROJECT_PATH> --workspace`
            // gets) so a mistyped invocation is not mistaken for adding the
            // extra root.
            if !cli.workspace.is_empty() {
                eprintln!(
                    "offcut: init <WORKSPACE> uses the positional workspace; --workspace {}",
                    if cli.workspace.len() == 1 {
                        format!("{} is ignored", cli.workspace[0])
                    } else {
                        format!("({} paths) is ignored", cli.workspace.len())
                    }
                );
            }
            if let Err(e) = run_init(&cli, workspace_path) {
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

/// Exit code returned by a JSON run: 0 success, 1 error, 3
/// approval-required. Approval-required uses 3 (not 2) so it is distinct
/// from clap's own usage-error exit code, which fires before `--json` is
/// even known to the program and produces no JSON document.
pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_APPROVAL_REQUIRED: i32 = 3;

/// Dispatch a `--json` invocation to the matching JSON emitter and return the
/// exit code. Each emitter prints exactly one JSON document on stdout and
/// any diagnostics on stderr.
///
/// The `--workspace`-ignored notice for a targeted clean still goes to
/// stderr under JSON mode — it is a diagnostic, not part of the document.
fn run_json(cli: &Cli) -> i32 {
    let command = json_command_name(cli);
    match &cli.command {
        None => json_clean(cli, None, command),
        Some(Command::List) => json_listing(cli, command),
        Some(Command::Config) => json_config(cli, command),
        Some(Command::Ignore { path }) => json_ignore(path, command),
        Some(Command::Safelist { path }) => json_safelist(cli, path, command),
        Some(Command::Discovery) => json_discovery(cli, command),
        Some(Command::Classification) => json_classification(cli, command),
        Some(Command::Clean { project_path }) => {
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
            json_clean(cli, project_path.as_deref(), command)
        }
        Some(Command::Init { workspace_path }) => {
            if !cli.workspace.is_empty() {
                eprintln!(
                    "offcut: init <WORKSPACE> uses the positional workspace; --workspace {}",
                    if cli.workspace.len() == 1 {
                        format!("{} is ignored", cli.workspace[0])
                    } else {
                        format!("({} paths) is ignored", cli.workspace.len())
                    }
                );
            }
            json_init(cli, workspace_path, command)
        }
    }
}

/// The `command` field for the JSON envelope. The default run reports
/// `"clean"` because it runs the clean flow.
fn json_command_name(cli: &Cli) -> &'static str {
    match &cli.command {
        None => "clean",
        Some(Command::List) => "list",
        Some(Command::Config) => "config",
        Some(Command::Ignore { .. }) => "ignore",
        Some(Command::Safelist { .. }) => "safelist",
        Some(Command::Discovery) => "discovery",
        Some(Command::Classification) => "classification",
        Some(Command::Clean { .. }) => "clean",
        Some(Command::Init { .. }) => "init",
    }
}

/// Print a JSON error document and return the error exit code.
fn json_err(command: &'static str, e: Box<dyn std::error::Error>) -> i32 {
    eprintln!("offcut: {e}");
    json::print(&json::err::<json::ListResult>(command, e.to_string()));
    EXIT_ERROR
}

/// `list --json`: each project's status, rank, and (for cleanable rows)
/// reclaimable size, plus the aggregate.
fn json_listing(cli: &Cli, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let (_config_path, cfg) = load_cli_config(cli)?;
        let cfg = cfg.apply_overrides(&cli_overrides(cli));
        let projects = discovery::discover(&cfg)?;
        let classified = classify_projects(&projects);
        let mut rows: Vec<(PathBuf, classify::Status)> =
            classified.iter().map(|(p, s, _)| (p.clone(), *s)).collect();
        rows.sort_by_key(|&(_, s)| s);
        let mut per_project_bytes: Vec<Option<u64>> = vec![None; rows.len()];
        for (idx, (path, status)) in rows.iter().enumerate() {
            if *status != classify::Status::Cleanable {
                continue;
            }
            let safe_set = match safelist::SafeSet::from_config(path, &cfg) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let ignore_set = match ignore::IgnoreSet::load(path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if let Ok(items) = clean::dry_run(path, &ignore_set, &safe_set)
                && let Ok(bytes) = disk::compute_reclaimable_size(path, &items)
            {
                per_project_bytes[idx] = Some(bytes);
            }
        }
        let cleanable_count = rows
            .iter()
            .filter(|(_, s)| *s == classify::Status::Cleanable)
            .count();
        let total: u64 = per_project_bytes.iter().filter_map(|x| *x).sum();
        let project_rows = rows
            .iter()
            .enumerate()
            .map(|(idx, (path, status))| json::ProjectRow {
                path: path.clone(),
                status: json::status_label(*status),
                rank: status.rank(),
                reclaimable_bytes: per_project_bytes[idx],
            })
            .collect();
        Ok(json::ok(
            command,
            json::ListResult {
                projects: project_rows,
                cleanable_count,
                total_reclaimable_bytes: total,
            },
        ))
    })();
    match result {
        Ok(env) => {
            json::print(&env);
            EXIT_OK
        }
        Err(e) => json_err(command, e),
    }
}

/// `classification --json`: each project's status and rank, sorted by
/// severity. No reclaimable size (classification is a read-only status survey).
fn json_classification(cli: &Cli, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let (_config_path, cfg) = load_cli_config(cli)?;
        let cfg = cfg.apply_overrides(&cli_overrides(cli));
        let projects = discovery::discover(&cfg)?;
        let classified = classify_projects(&projects);
        let mut rows: Vec<(PathBuf, classify::Status)> =
            classified.iter().map(|(p, s, _)| (p.clone(), *s)).collect();
        rows.sort_by_key(|&(_, s)| s);
        let project_rows = rows
            .iter()
            .map(|(path, status)| json::ProjectRow {
                path: path.clone(),
                status: json::status_label(*status),
                rank: status.rank(),
                reclaimable_bytes: None,
            })
            .collect();
        Ok(json::ok(
            command,
            json::ClassificationResult {
                projects: project_rows,
            },
        ))
    })();
    match result {
        Ok(env) => {
            json::print(&env);
            EXIT_OK
        }
        Err(e) => json_err(command, e),
    }
}

/// `discovery --json`: each discovered project path and its marker.
fn json_discovery(cli: &Cli, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let (_config_path, cfg) = load_cli_config(cli)?;
        let cfg = cfg.apply_overrides(&cli_overrides(cli));
        let projects = discovery::discover(&cfg)?;
        let rows = projects
            .iter()
            .map(|p| json::DiscoveryProject {
                path: p.path.clone(),
                marker: p.marker.clone(),
            })
            .collect();
        Ok(json::ok(command, json::DiscoveryResult { projects: rows }))
    })();
    match result {
        Ok(env) => {
            json::print(&env);
            EXIT_OK
        }
        Err(e) => json_err(command, e),
    }
}

/// `config --json`: the resolved configuration plus the per-invocation flags.
fn json_config(cli: &Cli, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let (config_path, cfg) = load_cli_config(cli)?;
        let overrides = cli_overrides(cli);
        let cfg = cfg.apply_overrides(&overrides);
        Ok(json::ok(
            command,
            json::ConfigResult {
                config_file: config_path,
                default_mode: cfg.default_mode.to_string(),
                max_depth: cfg.max_depth,
                workspace_roots: cfg.workspace_roots,
                safe_delete: cfg.safe_delete,
                project_markers: cfg.project_markers,
                force: overrides.force,
                dry_run: overrides.dry_run,
                verbose: overrides.verbose,
            },
        ))
    })();
    match result {
        Ok(env) => {
            json::print(&env);
            EXIT_OK
        }
        Err(e) => json_err(command, e),
    }
}

/// `ignore --json`: whether `path` is ignored by the loaded `.offcutignore`.
fn json_ignore(path: &str, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let root = std::env::current_dir()?;
        let set = ignore::IgnoreSet::load(&root)?;
        let p = std::path::Path::new(path);
        let rel = project_relative(&root, p)?;
        let ignored = set.is_ignored(rel);
        Ok(json::ok(
            command,
            json::IgnoreResult {
                path: path.to_string(),
                ignored,
            },
        ))
    })();
    match result {
        Ok(env) => {
            json::print(&env);
            EXIT_OK
        }
        Err(e) => json_err(command, e),
    }
}

/// `safelist --json`: whether `path` is safe to delete per the loaded catalog.
fn json_safelist(cli: &Cli, path: &str, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let root = std::env::current_dir()?;
        let p = std::path::Path::new(path);
        let rel = project_relative(&root, p)?;
        let (_config_path, cfg) = load_cli_config(cli)?;
        let set = safelist::SafeSet::from_config(&root, &cfg)?;
        let safe = set.is_safe(rel);
        Ok(json::ok(
            command,
            json::SafelistResult {
                path: path.to_string(),
                safe,
            },
        ))
    })();
    match result {
        Ok(env) => {
            json::print(&env);
            EXIT_OK
        }
        Err(e) => json_err(command, e),
    }
}

/// `init --json`: the created config file path.
fn json_init(cli: &Cli, workspace: &PathBuf, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let target = match &cli.config {
            Some(p) => p.clone(),
            None => {
                config::default_config_path().ok_or("could not determine platform config dir")?
            }
        };
        if target.symlink_metadata().is_ok() {
            if target.is_file() {
                return Err(format!("config file already exists: {}", target.display()).into());
            }
            return Err(format!("target exists and is not a file: {}", target.display()).into());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let resolved = if workspace.exists() {
            std::fs::canonicalize(workspace)?
                .to_string_lossy()
                .to_string()
        } else {
            workspace.to_string_lossy().to_string()
        };
        let template = generate_init_template(&resolved);
        fs::write(&target, &template)?;
        Ok(json::ok(
            command,
            json::InitResult {
                config_file: target,
                created: true,
            },
        ))
    })();
    match result {
        Ok(env) => {
            json::print(&env);
            EXIT_OK
        }
        Err(e) => json_err(command, e),
    }
}

/// `clean --json` (and the default run): enumerate, classify, and either
/// preview (`--dry-run`), auto-approve-and-execute (`--force`), or report
/// `approval_required` (neither) without deleting or prompting.
///
/// The compute path mirrors `run_cleaning`'s sizing/classification but emits
/// JSON instead of the human rendering. Progress is suppressed by the JSON
/// gate, so no live lines reach stdout.
fn json_clean(cli: &Cli, project_path: Option<&std::path::Path>, command: &'static str) -> i32 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let (_config_path, cfg) = load_cli_config(cli)?;
        let cfg = cfg.apply_overrides(&cli_overrides(cli));
        let projects = match project_path {
            Some(p) => vec![discovery::discover_single(p, &cfg)?],
            None => discovery::discover(&cfg)?,
        };
        let mut all_projects = classify_projects(&projects);
        all_projects.sort_by_key(|(_, status, _)| *status);

        // Per-cleanable-project sizing + enumeration, mirroring run_cleaning's
        // single pass. Items and bytes are reused for the result rows.
        let mut per_project_items: Vec<Option<Vec<clean::CleanItem>>> =
            vec![None; all_projects.len()];
        let mut per_project_bytes: Vec<Option<u64>> = vec![None; all_projects.len()];
        let mut per_project_safe_set: Vec<Option<safelist::SafeSet>> =
            vec![None; all_projects.len()];
        for (idx, (path, status, ignore_set)) in all_projects.iter().enumerate() {
            if *status != classify::Status::Cleanable {
                continue;
            }
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
            match clean::dry_run(path, ignore_set, &safe_set) {
                Ok(items) => {
                    if let Ok(bytes) = disk::compute_reclaimable_size(path, &items) {
                        per_project_bytes[idx] = Some(bytes);
                    }
                    per_project_items[idx] = Some(items);
                    per_project_safe_set[idx] = Some(safe_set);
                }
                Err(e) => eprintln!("warning: {}: dry_run failed: {e}", path.display()),
            }
        }

        let interactive = !cli.force && !cli.dry_run;

        let mut project_rows: Vec<json::CleanProjectRow> = Vec::new();
        // Whether any cleanable project actually survived sizing with enumerable
        // items. `approval_required` is computed from this — not from the raw
        // classification pass — so a cleanable project whose safe set or dry-run
        // failed (malformed `safe_delete`, unreadable tree) does not produce
        // `approval_required: true` with nothing to act on. Mirrors
        // `run_cleaning`'s `cleanable_items.is_empty()` no-op check.
        let mut any_actionable_cleanable = false;
        for (idx, (path, status, ignore_set)) in all_projects.iter().enumerate() {
            if *status != classify::Status::Cleanable {
                project_rows.push(json::CleanProjectRow {
                    path: path.clone(),
                    status: json::status_label(*status),
                    approved: false,
                    deleted_count: 0,
                    reclaimable_bytes: None,
                    items: Vec::new(),
                });
                continue;
            }
            let (items, safe_set) = match (
                per_project_items[idx].take(),
                per_project_safe_set[idx].take(),
            ) {
                (Some(items), Some(safe_set)) => (items, safe_set),
                // The project was classified cleanable but its sizing/enumeration
                // failed (warning already on stderr). Report it as a cleanable
                // row with no actionable items rather than dropping it silently,
                // so a caller can see it was found but nothing is deletable.
                _ => {
                    project_rows.push(json::CleanProjectRow {
                        path: path.clone(),
                        status: json::status_label(*status),
                        approved: false,
                        deleted_count: 0,
                        reclaimable_bytes: None,
                        items: Vec::new(),
                    });
                    continue;
                }
            };
            any_actionable_cleanable = true;

            // Determine approvals. Under --force every non-Protected item is
            // approved; under --dry-run nothing is executed (Safe would-delete,
            // Surfaced would-prompt); under the approval-required path nothing
            // is approved and nothing is executed.
            let approved_surfaced: Vec<PathBuf> = if cli.force {
                items
                    .iter()
                    .filter(|i| i.classification == clean::Classification::Surfaced)
                    .map(|i| i.rel_path.clone())
                    .collect()
            } else {
                Vec::new()
            };
            let will_execute = cli.force && !cli.dry_run;

            let would_delete: Vec<PathBuf> = items
                .iter()
                .filter(|item| {
                    if cli.dry_run && !cli.force {
                        item.classification == clean::Classification::Safe
                    } else {
                        matches!(item.classification, clean::Classification::Safe)
                            || (item.classification == clean::Classification::Surfaced
                                && (cli.force || approved_surfaced.contains(&item.rel_path)))
                    }
                })
                .map(|i| i.rel_path.clone())
                .collect();

            let deleted_count = if will_execute { would_delete.len() } else { 0 };

            // Execute the deletion under --force (no --dry-run). Mirrors
            // run_cleaning's execution step.
            if will_execute {
                let approved = approved_surfaced.clone();
                let outcome =
                    clean::clean(path, ignore_set, &safe_set, &approved, cli.force, false);
                if let Err(e) = outcome {
                    eprintln!("clean {}: failed: {e}", path.display());
                }
            }

            let item_rows = items
                .iter()
                .map(|item| json::CleanItemRow {
                    path: item.rel_path.clone(),
                    is_dir: item.is_dir,
                    classification: json::classification_label(item.classification),
                    fate: clean_item_fate(
                        item,
                        &would_delete,
                        will_execute,
                        interactive,
                        cli.dry_run,
                        cli.force,
                    ),
                })
                .collect();

            project_rows.push(json::CleanProjectRow {
                path: path.clone(),
                status: json::status_label(*status),
                approved: will_execute,
                deleted_count,
                reclaimable_bytes: per_project_bytes[idx],
                items: item_rows,
            });
        }

        let approval_required = interactive && any_actionable_cleanable;

        let env = json::Envelope {
            version: json::VERSION,
            command,
            ok: !approval_required,
            result: Some(json::CleanResult {
                approval_required,
                projects: project_rows,
            }),
            error: None,
        };
        Ok((env, approval_required))
    })();
    match result {
        Ok((env, approval_required)) => {
            json::print(&env);
            if approval_required {
                EXIT_APPROVAL_REQUIRED
            } else {
                EXIT_OK
            }
        }
        Err(e) => json_err(command, e),
    }
}

/// The fate string for one item in a JSON clean result. Mirrors the human
/// `item_fate` rule but returns the documented JSON vocabulary.
fn clean_item_fate(
    item: &clean::CleanItem,
    would_delete: &[PathBuf],
    will_execute: bool,
    interactive: bool,
    dry_run: bool,
    force: bool,
) -> &'static str {
    if would_delete.contains(&item.rel_path) {
        if will_execute {
            return "deleted";
        }
        return "would-delete";
    }
    if dry_run && !force && item.classification == clean::Classification::Surfaced {
        return "would-prompt";
    }
    if interactive && item.classification == clean::Classification::Surfaced {
        return "would-prompt";
    }
    "kept"
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

fn git_text(project_path: &Path, args: &[&str]) -> Option<String> {
    let out = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_path)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn branch_label(project_path: &Path, status: classify::Status) -> String {
    if status == classify::Status::NoGit {
        return "-".to_string();
    }
    git_text(project_path, &["branch", "--show-current"])
        .or_else(|| {
            git_text(project_path, &["rev-parse", "--short", "HEAD"])
                .map(|h| format!("detached@{h}"))
        })
        .unwrap_or_else(|| "-".to_string())
}

fn changed_label(project_path: &Path, status: classify::Status) -> String {
    if status == classify::Status::NoGit {
        return "-".to_string();
    }
    git_text(project_path, &["log", "-1", "--format=%cr"]).unwrap_or_else(|| "-".to_string())
}

/// Per-project branch labels, read from `git` at most once each.
///
/// The workspace table already reads every project's branch for its BRANCH
/// column; the review and blocked panels need the same string. Seeding the
/// cache from the table's read keeps a rendered run at one `git branch`
/// invocation per project instead of one per panel that mentions it.
struct BranchLabels {
    labels: Vec<Option<String>>,
}

impl BranchLabels {
    fn new(len: usize) -> Self {
        Self {
            labels: vec![None; len],
        }
    }

    fn seed(&mut self, labels: Vec<String>) {
        self.labels = labels.into_iter().map(Some).collect();
    }

    fn get(&mut self, idx: usize, project_path: &Path, status: classify::Status) -> String {
        match self.labels.get(idx) {
            Some(Some(label)) => label.clone(),
            Some(None) => {
                let label = branch_label(project_path, status);
                self.labels[idx] = Some(label.clone());
                label
            }
            None => branch_label(project_path, status),
        }
    }
}

/// Whether `project_path` has any commit at all.
///
/// `Status::Unpushed` is the one status that does not answer this on its own:
/// `classify::status_unpushed` short-circuits to true on a missing `HEAD`, so
/// the status covers both a branch whose commits were never pushed and a repo
/// that has never committed anything. The two need different copy, so the fact
/// is read from the same `git` plumbing every other label here uses.
fn has_commits(project_path: &Path) -> bool {
    git_text(project_path, &["rev-parse", "--verify", "HEAD"]).is_some()
}

/// The commit fact every panel's copy is read against, for one project.
///
/// Only `Unpushed` copy turns on it, so it is the only status that pays for the
/// extra plumbing call; every other status is committed as far as any panel is
/// concerned. One owner for the shortcut means the branch state and the refusal
/// beneath it are read from the same answer rather than two independent ones.
fn committed_state(project_path: &Path, status: classify::Status) -> bool {
    status != classify::Status::Unpushed || has_commits(project_path)
}

/// The one-line tree/remote state under `project_path`'s panel header.
///
/// Reads the one fact the status cannot supply, then hands off to
/// `format_branch_state`. A caller that also renders the blocked panel should
/// read `committed_state` once and call `format_branch_state` directly instead,
/// so both lines answer from one `git` call.
fn branch_state_line(project_path: &Path, branch: &str, status: classify::Status) -> String {
    format_branch_state(branch, status, committed_state(project_path, status))
}

/// Render the tree/remote state from facts already gathered.
///
/// It reports the two facts the header needs — the working tree and the remote
/// — in their own vocabulary, and only the ones actually established:
/// `Unpushed` is decided before `Wip` (see `classify::Status`), so an unpushed
/// project may still have uncommitted work and claiming a clean tree for it
/// would be a lie; `committed` is false for a repo with no commits, which must
/// not be described as having local commits ahead of anything.
///
/// Deliberately none of these arms repeat `interactive::status_reason`. The
/// blocked panel prints this line and that reason two lines apart, so a verbatim
/// copy would both make the panel say the same thing twice and leave one
/// sentence owned by two modules, free to drift.
fn format_branch_state(branch: &str, status: classify::Status, committed: bool) -> String {
    let (tree, remote) = match status {
        classify::Status::Cleanable | classify::Status::Clean => ("clean tree", "remote ✓ pushed"),
        classify::Status::Wip => ("uncommitted changes", "remote ✓"),
        classify::Status::Unpushed if committed => ("local commits ahead", "remote ✗ not pushed"),
        classify::Status::Unpushed => ("no commits yet", "remote ✗ nothing pushed"),
        classify::Status::NoRemote => ("local only", "remote ✗ none"),
        classify::Status::NoGit => return "-".to_string(),
    };
    format!("{branch} · {tree} · {remote}")
}

/// Render the rich workspace-summary table for `rows` on stdout.
///
/// The BRANCH and CHANGED columns each need their own `git` invocation per
/// project, so the reads run under a counted `reading N/M` progress phase —
/// otherwise a workspace with hundreds of projects pauses silently between
/// the sizing phase and the table. Shared by `run_listing` and `run_cleaning`
/// so the two renderings cannot drift.
///
/// Returns the branch label read for each row, in row order, so a caller that
/// renders further panels reuses them instead of re-spawning `git`.
///
/// `targeted` says whether the rows came from `discovery::discover_single`
/// rather than a workspace walk: that run never reads a configured workspace
/// root, so the header must not claim a scan that did not happen.
fn render_workspace_table(
    rows: &[(PathBuf, classify::Status)],
    sizes: &[Option<String>],
    cleanable_count: usize,
    total_reclaimable: Option<&str>,
    targeted: bool,
    emit_colors: bool,
) -> Vec<String> {
    let mut progress = progress::ProgressWriter::new(std::io::stdout());
    let mut branches: Vec<String> = Vec::with_capacity(rows.len());
    let mut changed: Vec<String> = Vec::with_capacity(rows.len());
    for (i, (path, status)) in rows.iter().enumerate() {
        progress.update_phase("reading", i + 1, rows.len(), path);
        branches.push(branch_label(path, *status));
        changed.push(changed_label(path, *status));
    }
    progress.finish();

    let table_rows: Vec<output::ProjectTableRow<'_>> = rows
        .iter()
        .enumerate()
        .map(|(idx, (path, status))| output::ProjectTableRow {
            path,
            status: *status,
            size: sizes.get(idx).and_then(|s| s.as_deref()),
            branch: Some(branches[idx].as_str()),
            changed: Some(changed[idx].as_str()),
        })
        .collect();
    let width = output::terminal_width();
    for line in output::wrap_line(
        &format!(
            "⟩ {} · {} {}",
            if targeted {
                "inspected the requested project"
            } else {
                "scanned configured workspaces"
            },
            rows.len(),
            output::projects_word(rows.len())
        ),
        width,
    ) {
        println!("{line}");
    }
    for line in output::format_project_table(
        &table_rows,
        cleanable_count,
        total_reclaimable,
        width,
        emit_colors,
    ) {
        println!("{line}");
    }
    branches
}

/// The panel a targeted `offcut clean <PROJECT_PATH>` ends on when the run
/// found nothing to clean, or `None` when no panel states this outcome
/// truthfully.
///
/// A blocking tree state gets the refusal, the action that unblocks it, and
/// what cleaning would free once it is unblocked. An already-clean project gets
/// the nothing-to-reclaim state instead: it is committed, pushed, and carries
/// nothing offcut may delete, so "commit, push, or initialize as needed" is
/// advice it cannot act on. A `Cleanable` project only reaches here when its
/// own inspection failed — the sizing pass already warned about that on
/// stderr, and no panel would be honest about it.
///
/// `committed` is the same `committed_state` answer `branch_line` was rendered
/// from, so the refusal and the branch state cannot disagree about whether the
/// project has commits.
fn targeted_outcome_panel(
    path: &Path,
    status: classify::Status,
    committed: bool,
    branch_line: &str,
    details: &[String],
    possible_reclaim: Option<&str>,
    width: usize,
    emit_colors: bool,
) -> Option<Vec<String>> {
    if output::blocks_cleaning(status) {
        return Some(output::format_blocked_project(
            path,
            status,
            committed,
            branch_line,
            details,
            possible_reclaim,
            width,
            emit_colors,
        ));
    }
    if status == classify::Status::Clean {
        return Some(output::format_nothing_to_reclaim(
            path,
            branch_line,
            width,
            emit_colors,
        ));
    }
    None
}

/// What cleaning `project_path` would free if its tree stopped blocking, or
/// `None` when the figure cannot be measured or would be zero.
///
/// Reuses the sizing pass's read-only pipeline — safe set, `clean::dry_run`,
/// `disk::compute_reclaimable_size` — but counts `Safe` items only. A blocked
/// project's `Surfaced` items are untracked paths that are neither protected
/// nor safe-listed, i.e. routinely the user's own new work; the very action
/// this panel demands (commit and push) makes them tracked, so offcut would
/// never delete them and quoting their bytes promises a reclaim that can never
/// arrive. Safe-listed build output is what cleaning takes without asking, so
/// it is the only part of the figure the panel can stand behind.
///
/// Only the one targeted project is measured: a workspace run never reaches
/// this, so the listing keeps its cost. Every failure (no repo to enumerate,
/// an unbuildable safe set) degrades to no figure rather than to an error —
/// the panel's subject is the blocked tree, not the measurement.
fn blocked_reclaim(
    project_path: &Path,
    ignore_set: &ignore::IgnoreSet,
    cfg: &Config,
) -> Option<String> {
    let mut progress = progress::ProgressWriter::new(std::io::stdout());
    progress.update_phase("sizing", 1, 1, project_path);
    let measured = safelist::SafeSet::from_config(project_path, cfg)
        .ok()
        .and_then(|safe_set| clean::dry_run(project_path, ignore_set, &safe_set).ok())
        .and_then(|items| {
            let safe_only: Vec<clean::CleanItem> = items
                .into_iter()
                .filter(|item| item.classification == clean::Classification::Safe)
                .collect();
            disk::compute_reclaimable_size(project_path, &safe_only).ok()
        })
        .filter(|&bytes| bytes > 0)
        .map(disk::format_size);
    progress.finish();
    measured
}

fn status_detail_lines(project_path: &Path, status: classify::Status) -> Vec<String> {
    if status != classify::Status::Wip {
        return Vec::new();
    }
    git_text(project_path, &["status", "--porcelain"])
        .map(|s| s.lines().take(6).map(str::to_string).collect())
        .unwrap_or_default()
}

/// `offcut list`: show each discovered project with its git status, sorted
/// by severity (most-needs-attention first). Read-only — no cleaning.
///
/// Each row uses the formatted shape `[rank] path — label (reason)` with
/// color coding per status. Cleanable rows carry a bold-cyan label so the
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
    let mut rows: Vec<(PathBuf, classify::Status)> = classify_projects(&projects)
        .into_iter()
        .map(|(path, status, _)| (path, status))
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
            let path = rows[idx].0.as_path();
            progress.update_phase("sizing", i + 1, m, path);
            let safe_set = match safelist::SafeSet::from_config(path, &cfg) {
                Ok(s) => s,
                Err(e) => {
                    progress.clear();
                    eprintln!(
                        "warning: {}: could not build safe-to-delete set: {e}",
                        rows[idx].0.display()
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
                            rows[idx].0.display()
                        );
                    }
                },
                Err(e) => {
                    progress.clear();
                    eprintln!("warning: {}: dry_run failed: {e}", rows[idx].0.display());
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

    if output::stdout_terminal_ui_enabled() {
        render_workspace_table(
            &rows,
            &per_project_size,
            cleanable_count,
            total_reclaimable_str.as_deref(),
            false,
            output::stdout_color_enabled(),
        );
    } else {
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
            println!("{}", output::format_project_row(path, *status, None, size,));
        }
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
    let mut rows: Vec<(PathBuf, classify::Status)> = classify_projects(&projects)
        .into_iter()
        .map(|(path, status, _)| (path, status))
        .collect();
    rows.sort_by_key(|&(_, status)| status);

    println!(
        "classification: {} project(s), sorted by severity",
        rows.len()
    );
    for (path, status) in &rows {
        println!(
            "  [{}] {} -> {}",
            status.rank(),
            path.display(),
            status.label()
        );
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
/// Every top-level flag (`--force`, `--dry-run`, `--config`) still applies
/// to the targeted project, unchanged by path targeting (`--verbose` is
/// accepted here too, and produces no additional output, as elsewhere).
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
    let mut cleanable_items: Vec<interactive::CleanableProject> = Vec::new();
    let mut cleanable_meta: Vec<(usize, safelist::SafeSet)> = Vec::new();
    let rich_stdout = output::stdout_terminal_ui_enabled();
    let emit_colors = output::stdout_color_enabled();
    // The interactive flow needs the same (path, status) pairs the table
    // renders, so the projection is built once and shared.
    let project_statuses: Vec<(PathBuf, classify::Status)> = all_projects
        .iter()
        .map(|(p, s, _)| (p.clone(), *s))
        .collect();
    // Whether the interactive flow *may* draw the pre-approval review panel on
    // stderr. It is only a prediction — the flow skips the panel outright when
    // the user declines the all-cleanup prompt — so it gates nothing but the
    // branch read below, which has to happen before the flow runs. What was
    // actually rendered comes back per project as `ProjectResult::review_shown`.
    let stderr_may_render_review =
        !cli.force && !cli.dry_run && output::stderr_terminal_ui_enabled();
    let mut branch_labels = BranchLabels::new(all_projects.len());
    if rich_stdout {
        branch_labels.seed(render_workspace_table(
            &project_statuses,
            &per_project_size,
            cleanable_count,
            total_reclaimable_str.as_deref(),
            project_path.is_some(),
            emit_colors,
        ));
    } else {
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
    }
    for (idx, (path, status, _ignore_set)) in all_projects.iter().enumerate() {
        if *status == classify::Status::Cleanable {
            let (items, safe_set) = match (
                per_project_items[idx].take(),
                per_project_safe_set[idx].take(),
            ) {
                (Some(items), Some(safe_set)) => (items, safe_set),
                _ => continue,
            };
            if !rich_stdout {
                println!(
                    "{}",
                    output::format_project_row(
                        path,
                        *status,
                        None,
                        per_project_size[idx].as_deref()
                    )
                );
            }
            // The branch line only reaches the screen through the pre-approval
            // review panel, so it is read only for the runs that render one.
            let branch_line = if stderr_may_render_review {
                branch_state_line(path, &branch_labels.get(idx, path, *status), *status)
            } else {
                String::new()
            };
            cleanable_items.push(interactive::CleanableProject {
                path: path.clone(),
                items,
                branch_line,
                total_size: per_project_size[idx].clone(),
            });
            cleanable_meta.push((idx, safe_set));
        } else if !rich_stdout {
            println!("{}", output::format_project_row(path, *status, None, None));
        }
    }

    // Zero cleanable: summary and exit 0 — no prompts, no enumeration,
    // nothing to clean. The flow only runs when there is a subject to clean.
    if cleanable_items.is_empty() {
        let panel = if rich_stdout && project_path.is_some() {
            all_projects.first().map(|(path, status, ignore_set)| {
                let committed = committed_state(path, *status);
                let branch =
                    format_branch_state(&branch_labels.get(0, path, *status), *status, committed);
                let reclaim = if output::blocks_cleaning(*status) {
                    blocked_reclaim(path, ignore_set, &cfg)
                } else {
                    None
                };
                targeted_outcome_panel(
                    path,
                    *status,
                    committed,
                    &branch,
                    &status_detail_lines(path, *status),
                    reclaim.as_deref(),
                    output::terminal_width(),
                    emit_colors,
                )
            })
        } else {
            None
        };
        match panel.flatten() {
            Some(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
            // No panel fits this outcome — an empty project list (every
            // project skipped by `classify_projects`, warned about on stderr)
            // or a cleanable project whose inspection failed. Either way the
            // run still says what it did: bounded on a rendering terminal,
            // and on one line for the scripts reading the plain stream.
            None => {
                let summary = "clean: no cleanable projects — nothing to delete";
                if rich_stdout {
                    for line in output::wrap_line(summary, output::terminal_width()) {
                        println!("{line}");
                    }
                } else {
                    println!("{summary}");
                }
            }
        }
        return Ok(());
    }

    // Build the interactive inputs and run the state machine against the
    // process's stdin (locked, buffered for line reads). The flow itself
    // owns the decision state machine; the CLI hook owns the I/O plumbing.
    let inputs = interactive::InteractiveFlowInputs {
        all_projects: project_statuses,
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
    //
    // A project whose review the flow already showed on stderr — immediately
    // before the question it belongs to — must not have it repeated here after
    // the decision was made. Every other project (--force, --dry-run, a stderr
    // that cannot render it, or a declined all-cleanup prompt that skipped the
    // per-project report entirely) gets the panel on stdout, so no outcome is
    // reported without the project and items it is about.
    let mut clean_progress = progress::ProgressWriter::new(std::io::stdout());
    let width = output::terminal_width();
    for (i, (r, (idx, safe_set))) in results.iter().zip(&cleanable_meta).enumerate() {
        let will_execute = enters_cleaning_execution_phase(r.project_approved, cli.dry_run);
        // What this run will actually do to each item, once the approvals are
        // in. Shared by the plain listing and the rich panel so neither can
        // claim a fate the other contradicts.
        let fate_of = |item: &clean::CleanItem| -> &'static str {
            item_fate(
                item,
                &r.would_delete,
                r.project_approved,
                cli.force,
                cli.dry_run,
            )
        };
        clean_progress.clear();
        if rich_stdout {
            if !r.review_shown {
                let rows = output::clean_review_rows(&r.items, fate_of);
                let branch = branch_labels.get(*idx, &r.path, r.status);
                for line in output::format_clean_review(
                    &r.path,
                    &branch_state_line(&r.path, &branch, r.status),
                    &rows,
                    per_project_size[*idx].as_deref(),
                    width,
                    emit_colors,
                ) {
                    println!("{line}");
                }
            }
            if r.project_approved {
                if will_execute {
                    println!(
                        "{}",
                        output::format_path_line("⟩ cleaning ", &r.path, width)
                    );
                } else {
                    for line in output::wrap_line("⟩ dry-run only - nothing deleted", width) {
                        println!("{line}");
                    }
                }
            } else {
                println!(
                    "{}",
                    output::format_path_line("⟩ skipped by user · ", &r.path, width)
                );
            }
        } else {
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
                println!(
                    "  {}{} [{}] ({})",
                    item.rel_path.display(),
                    if item.is_dir { "/" } else { "" },
                    label,
                    fate_of(item)
                );
            }
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
            // What this run frees is what it deletes: the sizing pass measured
            // every deletable item, including any the user then declined, so
            // reporting that figure would overstate the reclaim. Re-measure
            // over the approved items only — and only when something was
            // actually declined, so the common case keeps its single pass.
            // Must happen before `clean` deletes the paths being measured.
            let reclaimed = if !rich_stdout {
                None
            } else if r.items.iter().any(|i| {
                matches!(
                    i.classification,
                    clean::Classification::Safe | clean::Classification::Surfaced
                ) && !r.would_delete.contains(&i.rel_path)
            }) {
                let deleted: Vec<clean::CleanItem> = r
                    .items
                    .iter()
                    .filter(|i| r.would_delete.contains(&i.rel_path))
                    .cloned()
                    .collect();
                disk::compute_reclaimable_size(&r.path, &deleted)
                    .ok()
                    .map(disk::format_size)
            } else {
                per_project_size[*idx].clone()
            };
            let ignore_set = &all_projects[*idx].2;
            clean_progress.update_phase("cleaning", i + 1, cleanable_meta.len(), &r.path);
            let outcome = clean::clean(&r.path, ignore_set, safe_set, &approved, cli.force, false);
            clean_progress.clear();
            match outcome {
                Ok(_) => {
                    if rich_stdout {
                        for line in output::format_clean_success(
                            &r.path,
                            r.would_delete.len(),
                            reclaimed.as_deref(),
                            width,
                            emit_colors,
                        ) {
                            println!("{line}");
                        }
                    } else {
                        println!(
                            "clean {}: deleted {} item(s)",
                            r.path.display(),
                            r.would_delete.len()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("clean {}: failed: {e}", r.path.display());
                }
            }
        }
    }
    Ok(())
}

// What this run will actually do to one item after approvals are known.
// Used by both plain output and rich review rows so neither can claim a fate
// the other contradicts.
fn item_fate(
    item: &clean::CleanItem,
    would_delete: &[PathBuf],
    project_approved: bool,
    force: bool,
    dry_run: bool,
) -> &'static str {
    let will_execute = enters_cleaning_execution_phase(project_approved, dry_run);
    if !project_approved && !dry_run {
        return "kept";
    }
    if would_delete.contains(&item.rel_path) {
        if will_execute {
            "deleting"
        } else {
            "would delete"
        }
    } else if dry_run && !force && item.classification == clean::Classification::Surfaced {
        // A real interactive run would ask about this item, so the preview
        // must not claim either fate.
        "would prompt"
    } else {
        "kept"
    }
}

// The live `cleaning N/M` progress phase belongs to the destructive execution
// step only. Dry-runs and declined projects still render outcomes, but they are
// not actively cleaning anything.
fn enters_cleaning_execution_phase(project_approved: bool, dry_run: bool) -> bool {
    project_approved && !dry_run
}

#[cfg(test)]
mod item_fate_tests {
    use super::*;

    fn item(path: &str, classification: clean::Classification) -> clean::CleanItem {
        clean::CleanItem {
            rel_path: PathBuf::from(path),
            is_dir: false,
            classification,
        }
    }

    fn rendered_fates(
        items: &[clean::CleanItem],
        would_delete: &[PathBuf],
        project_approved: bool,
    ) -> String {
        let rows = output::clean_review_rows(items, |item| {
            item_fate(item, would_delete, project_approved, false, false)
        });
        output::format_clean_review(
            Path::new("/workspace/app"),
            "main · clean tree · remote ✓ pushed",
            &rows,
            Some("10 KB"),
            100,
            false,
        )
        .join("\n")
    }

    /// Declining the all-cleanup prompt means no project was approved, so the
    /// post-decision rich review must report every item as kept even though
    /// the pre-decision plan included safe-listed paths in `would_delete`.
    #[test]
    fn declined_all_cleanup_rich_review_reports_every_item_kept() {
        let items = vec![
            item("target", clean::Classification::Safe),
            item("scratch.txt", clean::Classification::Surfaced),
        ];
        let would_delete = vec![PathBuf::from("target"), PathBuf::from("scratch.txt")];

        let rendered = rendered_fates(&items, &would_delete, false);

        assert!(rendered.contains("target"), "{rendered}");
        assert!(rendered.contains("scratch.txt"), "{rendered}");
        assert_eq!(
            rendered.matches("kept").count(),
            2,
            "declined real run keeps every item: {rendered}"
        );
        assert!(
            !rendered.contains("would delete"),
            "declined real run must not promise deletion: {rendered}"
        );
    }

    /// The same fate rule applies after a user reaches an individual project
    /// prompt and declines it: safe and surfaced entries alike are untouched.
    #[test]
    fn declined_per_project_rich_review_reports_every_item_kept() {
        let items = vec![
            item("node_modules", clean::Classification::Safe),
            item("coverage.tmp", clean::Classification::Surfaced),
            item("keep.dat", clean::Classification::Protected),
        ];
        let would_delete = vec![PathBuf::from("node_modules"), PathBuf::from("coverage.tmp")];

        let rendered = rendered_fates(&items, &would_delete, false);

        assert_eq!(
            rendered.matches("kept").count(),
            3,
            "declined project keeps safe, surfaced, and protected items: {rendered}"
        );
        assert!(!rendered.contains("would delete"), "{rendered}");
        assert!(!rendered.contains("deleting"), "{rendered}");
    }

    #[test]
    fn approved_project_still_reports_planned_deletions() {
        let items = vec![item("target", clean::Classification::Safe)];
        let would_delete = vec![PathBuf::from("target")];

        let rendered = rendered_fates(&items, &would_delete, true);

        assert!(rendered.contains("deleting"), "{rendered}");
    }

    #[test]
    fn live_cleaning_progress_is_only_for_real_approved_execution() {
        assert!(enters_cleaning_execution_phase(true, false));
        assert!(!enters_cleaning_execution_phase(true, true));
        assert!(!enters_cleaning_execution_phase(false, false));
        assert!(!enters_cleaning_execution_phase(false, true));
    }
}

// ---------------------------------------------------------------------------
// Targeted-run outcome panel: unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod targeted_panel_tests {
    use super::*;
    use std::fs;

    fn panel_with(status: classify::Status, possible_reclaim: Option<&str>) -> Option<String> {
        panel_for(status, possible_reclaim, true)
    }

    fn panel_for(
        status: classify::Status,
        possible_reclaim: Option<&str>,
        committed: bool,
    ) -> Option<String> {
        targeted_outcome_panel(
            Path::new("/workspace/dashboard"),
            status,
            committed,
            &format_branch_state("main", status, committed),
            &[],
            possible_reclaim,
            80,
            false,
        )
        .map(|lines| lines.join("\n"))
    }

    fn panel(status: classify::Status) -> Option<String> {
        panel_with(status, None)
    }

    /// A committed, pushed project with nothing left to delete is not blocked
    /// on anything: refusing to clean it and asking the user to commit and push
    /// would be advice they cannot act on.
    #[test]
    fn clean_project_reports_nothing_to_reclaim_instead_of_a_refusal() {
        let rendered = panel(classify::Status::Clean).expect("clean projects get a panel");
        assert!(rendered.contains("nothing to reclaim"), "{rendered}");
        assert!(!rendered.contains("refusing to clean"), "{rendered}");
        assert!(!rendered.contains("commit, push"), "{rendered}");
    }

    /// Every tree state the user can act on keeps the refusal and the rerun
    /// hint that unblocks it.
    #[test]
    fn blocking_states_keep_the_refusal_panel() {
        for status in [
            classify::Status::NoGit,
            classify::Status::NoRemote,
            classify::Status::Unpushed,
            classify::Status::Wip,
        ] {
            let rendered = panel(status).unwrap_or_else(|| panic!("{status:?} gets a panel"));
            assert!(rendered.contains("refusing to clean"), "{status:?}");
            assert!(rendered.contains("offcut clean"), "{status:?}");
        }
    }

    /// A cleanable project only reaches this branch when its own inspection
    /// failed — already warned about on stderr. No panel states that
    /// truthfully, so the caller falls back to the plain summary line.
    #[test]
    fn uninspectable_cleanable_project_gets_no_panel() {
        assert!(panel(classify::Status::Cleanable).is_none());
    }

    /// The blocked figure counts safe-listed build output only. A blocked
    /// project's surfaced items are untracked paths offcut has not been told
    /// it may delete — routinely the user's own new work — and the commit the
    /// panel asks for makes them tracked, so counting them promises a reclaim
    /// that can never arrive.
    #[test]
    fn blocked_reclaim_counts_safe_output_not_untracked_work() {
        let root = std::env::temp_dir().join(format!(
            "offcut-blocked-reclaim-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join("scratch")).unwrap();
        let git = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("init")
            .output()
            .unwrap();
        assert!(git.status.success());
        fs::write(root.join("node_modules/pkg.bin"), vec![b'x'; 2048]).unwrap();
        fs::write(root.join("scratch/dataset.csv"), vec![b'y'; 8192]).unwrap();

        let ignore_set = ignore::IgnoreSet::load(&root).unwrap();
        let measured = blocked_reclaim(&root, &ignore_set, &Config::default());

        assert_eq!(
            measured.as_deref(),
            Some(disk::format_size(2048).as_str()),
            "only node_modules may count; scratch/dataset.csv is the user's work"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// `Unpushed` covers a branch whose commits were never pushed *and* a repo
    /// that has never committed anything, so the panel states the one it is
    /// actually looking at rather than inventing commits for an empty repo.
    ///
    /// The refusal is checked alongside the branch state: the two lines are two
    /// apart, so a refusal reading "has unpushed commits" under a branch line
    /// reading "no commits yet" makes the panel argue with itself.
    #[test]
    fn unpushed_panel_does_not_claim_commits_an_empty_repo_lacks() {
        let empty = panel_for(classify::Status::Unpushed, None, false).expect("panel");
        assert!(empty.contains("no commits yet"), "{empty}");
        assert!(!empty.contains("commits ahead"), "{empty}");
        assert!(
            empty.contains("refusing to clean - nothing committed yet"),
            "{empty}"
        );
        assert!(!empty.contains("has unpushed commits"), "{empty}");
        assert!(empty.contains("commit, push"), "{empty}");

        let ahead = panel_for(classify::Status::Unpushed, None, true).expect("panel");
        assert!(ahead.contains("local commits ahead"), "{ahead}");
        assert!(!ahead.contains("no commits yet"), "{ahead}");
        assert!(
            ahead.contains("refusing to clean - has unpushed commits"),
            "{ahead}"
        );
        assert!(!ahead.contains("nothing committed"), "{ahead}");
    }

    /// The commit fact the unpushed copy turns on is read from git, not
    /// guessed: a freshly initialized repo has none, and one commit is enough.
    #[test]
    fn has_commits_reads_the_repository_not_the_status() {
        let root = std::env::temp_dir().join(format!(
            "offcut-has-commits-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init"]);
        assert!(
            !has_commits(&root),
            "a repo with no commits has nothing ahead of a remote"
        );

        git(&["config", "user.email", "test@test.dev"]);
        git(&["config", "user.name", "Test"]);
        fs::write(root.join("initial.txt"), "initial").unwrap();
        git(&["add", "initial.txt"]);
        git(&["commit", "-m", "initial"]);
        assert!(has_commits(&root));

        let _ = fs::remove_dir_all(&root);
    }

    /// A measured blocked project quotes what cleaning would free once the
    /// tree stops blocking; an unmeasurable one simply omits the figure.
    #[test]
    fn blocked_panel_carries_the_measured_reclaim_when_known() {
        let measured = panel_with(classify::Status::Wip, Some("540 MB")).expect("panel");
        assert!(measured.contains("~540 MB"), "{measured}");
        assert!(measured.contains("would become reclaimable"), "{measured}");

        let unmeasured = panel(classify::Status::Wip).expect("panel");
        assert!(
            !unmeasured.contains("would become reclaimable"),
            "{unmeasured}"
        );
    }
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
                json: false,
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
                json: false,
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
                json: false,
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
                json: false,
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
