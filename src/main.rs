use clap::Parser;

/// devclean - development environment cleanup CLI (scaffold)
#[derive(Parser, Debug)]
#[command(name = "devclean", version, about)]
struct Cli {
    // Intentionally minimal: no subcommands or options are defined yet.
    // Product behavior will be added as the project grows.
}

fn main() {
    let _cli = Cli::parse();
    println!("devclean - development environment cleanup CLI (scaffold)");
}