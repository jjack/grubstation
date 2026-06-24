mod config;
mod grub;
mod wizard;
mod service;
mod mdns;

use anyhow::Result;
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(config::get_default_config_path);

    match &cli.command {
        Commands::Init => {
            wizard::wizard_init(&config_path)?;
        }
        Commands::Validate => match config::load_config(&config_path) {
            Ok(_) => println!("Configuration is valid."),
            Err(e) => {
                eprintln!("Validation failed: {}", e);
                std::process::exit(1);
            }
        },
        Commands::Run => {
            println!("Starting the long-running process...");
            let config = config::load_config(&config_path).map_err(|e| anyhow::anyhow!("{}", e))?;
            let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(config::DEFAULT_DAEMON_PORT);

            let _mdns = mdns::start_advertisement(&config)?;
            println!("Grubstation daemon is running and advertising service via mDNS on port {}...", port);

            // Loop to keep the daemon alive
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        Commands::Sync { dry_run } => {
            let config = config::load_config(&config_path).map_err(|e| anyhow::anyhow!("{}", e))?;

            if let Some(ref grub_config) = config.grub {
                let entries = grub::parse_grub_entries(&grub_config.path)?;

                let payload = serde_json::json!({
                    "mac": config.host.mac,
                    "boot_options": entries,
                });

                if *dry_run {
                    eprintln!("Performing a dry-run sync: dumping formatted output to stdout...");
                    let formatted = serde_json::to_string_pretty(&payload)?;
                    println!("{}", formatted);
                } else {
                    println!("Syncing to Home Assistant...");
                    println!("Payload to send: {}", serde_json::to_string(&payload)?);
                }
            } else {
                anyhow::bail!("No GRUB configuration found to sync.");
            }
        }
    }

    Ok(())
}
