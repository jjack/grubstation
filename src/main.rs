use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "grubstation")]
#[command(about = "Grubstation CLI for configuration and synchronization", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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

    match &cli.command {
        Commands::Init => {
            println!("Running the wizard to scaffold your initial configuration file...");
            // TODO: Implement wizard logic
        }
        Commands::Validate => {
            println!("Validating configuration...");
            // TODO: Implement validation logic
        }
        Commands::Run => {
            println!("Starting daemon...");
            // TODO: Implement run logic
        }
        Commands::Sync { dry_run } => {
            if *dry_run {
                println!("Performing a dry-run sync: dumping formatted output to stdout/stderr...");
                // TODO: Implement dry-run logic
            } else {
                println!("Syncing to Home Assistant...");
                // TODO: Implement sync logic
            }
        }
    }
}
