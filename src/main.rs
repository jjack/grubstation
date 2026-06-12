mod config;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "grubstation")]
#[command(about = "Grubstation CLI for configuration and synchronization", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Runs the wizard to scaffold your initial configuration file.
    Init,
    /// Parses the config strictly for syntax and logical errors without executing anything.
    Validate,
    /// Explicitly starts the long-running process.
    Run,
    /// Parses the target file and fires the payload off to Home Assistant.
    Sync {
        /// Parses the target file and dumps the formatted output to stdout/stderr, bypassing the HTTP client entirely.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(config::get_default_config_path);

    match &cli.command {
        Commands::Init => {
            println!("Running the wizard to scaffold your initial configuration file...");
        }
        Commands::Validate => {
            match config::load_config(&config_path) {
                Ok(_) => println!("Configuration is valid."),
                Err(e) => {
                    eprintln!("Validation failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Run => {
            println!("Starting the long-running process...");
        }
        Commands::Sync { dry_run } => {
            if *dry_run {
                println!("Performing a dry-run sync: dumping formatted output to stdout/stderr...");
            } else {
                println!("Syncing to Home Assistant...");
            }
        }
    }
}
