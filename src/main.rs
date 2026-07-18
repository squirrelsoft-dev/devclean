mod config;

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

fn run_list(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = cli.config.clone().or_else(config::default_config_path);

    let cfg = match &config_path {
        Some(path) => Config::load_or_default(path)?,
        None => Config::default(),
    };
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
