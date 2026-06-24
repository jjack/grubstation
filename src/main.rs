mod config;
mod grub;
mod wizard;
mod service;
mod mdns;
mod server;

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

            let (mdns, service_info) = mdns::start_advertisement(&config)?;
            server::start_server(&config, config_path.clone(), mdns, service_info)?;
            println!("Grubstation daemon is running and advertising service via mDNS on port {}...", port);

            // Loop to keep the daemon alive
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        Commands::Sync { dry_run } => {
            run_sync(&config_path, *dry_run)?;
        }
    }

    Ok(())
}

pub fn run_sync(config_path: &std::path::Path, dry_run: bool) -> Result<()> {
    let config = config::load_config(config_path).map_err(|e| anyhow::anyhow!("{}", e))?;

    if let Some(ref grub_config) = config.grub {
        let entries = grub::parse_grub_entries(&grub_config.path)?;

        let payload = serde_json::json!({
            "action": "update_boot_options",
            "mac": config.host.mac,
            "boot_options": entries,
        });

        if dry_run {
            eprintln!("Performing a dry-run sync: dumping formatted output to stdout...");
            let formatted = serde_json::to_string_pretty(&payload)?;
            println!("{}", formatted);
        } else {
            let state_path = config_path.parent().unwrap_or(std::path::Path::new(".")).join("state.json");
            if !state_path.exists() {
                anyhow::bail!("Daemon is not paired. Please pair the daemon first or ensure state.json exists.");
            }
            let state_content = std::fs::read_to_string(&state_path)
                .map_err(|e| anyhow::anyhow!("Failed to read state.json: {}", e))?;
            let state_val: serde_json::Value = serde_json::from_str(&state_content)
                .map_err(|e| anyhow::anyhow!("Failed to parse state.json: {}", e))?;
            let paired = state_val["paired"].as_bool().unwrap_or(false);
            if !paired {
                anyhow::bail!("Daemon is not paired. Please pair the daemon first.");
            }
            let webhook_id = state_val["webhook_id"].as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing webhook_id in state.json"))?;
            let api_key = state_val["api_key"].as_str().unwrap_or("");
            let ha_daemon_url = state_val["ha_daemon_url"].as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing ha_daemon_url in state.json"))?;

            println!("Syncing to Home Assistant...");
            println!("Payload to send: {}", serde_json::to_string(&payload)?);
            crate::grub::push_boot_options(
                ha_daemon_url,
                webhook_id,
                api_key,
                &config.host.mac,
                &entries,
            )?;
            println!("Sync successful!");
        }
    } else {
        anyhow::bail!("No GRUB configuration found to sync.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_run_sync_dry_run() -> Result<()> {
        let dir = tempdir()?;
        let grub_path = dir.path().join("grub.cfg");
        let mut grub_file = File::create(&grub_path)?;
        writeln!(grub_file, "menuentry 'Ubuntu' {{")?;
        writeln!(grub_file, "  echo 1")?;
        writeln!(grub_file, "}}")?;
        grub_file.sync_all()?;

        let config_path = dir.path().join("config.yaml");
        let mut config_file = File::create(&config_path)?;
        writeln!(config_file, "host:")?;
        writeln!(config_file, "  mac: \"00:11:22:33:44:55\"")?;
        writeln!(config_file, "  address: \"127.0.0.1\"")?;
        writeln!(config_file, "grub:")?;
        writeln!(config_file, "  path: {:?}", grub_path)?;
        writeln!(config_file, "  network_wait: 10")?;
        writeln!(config_file, "  webhook_id: \"test-webhook\"")?;
        config_file.sync_all()?;

        // dry run should succeed without any state.json or server
        run_sync(&config_path, true)?;

        // real run without state.json should fail
        assert!(run_sync(&config_path, false).is_err());

        Ok(())
    }

    #[test]
    fn test_run_sync_real() -> Result<()> {
        let dir = tempdir()?;
        let grub_path = dir.path().join("grub.cfg");
        let mut grub_file = File::create(&grub_path)?;
        writeln!(grub_file, "menuentry 'Ubuntu' {{")?;
        writeln!(grub_file, "  echo 1")?;
        writeln!(grub_file, "}}")?;
        grub_file.sync_all()?;

        let config_path = dir.path().join("config.yaml");
        let mut config_file = File::create(&config_path)?;
        writeln!(config_file, "host:")?;
        writeln!(config_file, "  mac: \"00:11:22:33:44:55\"")?;
        writeln!(config_file, "  address: \"127.0.0.1\"")?;
        writeln!(config_file, "grub:")?;
        writeln!(config_file, "  path: {:?}", grub_path)?;
        writeln!(config_file, "  network_wait: 10")?;
        writeln!(config_file, "  webhook_id: \"test-webhook\"")?;
        config_file.sync_all()?;

        // Start mock HA server
        let ha_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let ha_port = ha_listener.local_addr()?.port();
        let ha_server = tiny_http::Server::from_listener(ha_listener, None)
            .map_err(|e| anyhow::anyhow!("Failed to start HA server: {}", e))?;

        let handle = std::thread::spawn(move || {
            if let Some(mut req) = ha_server.incoming_requests().next() {
                assert_eq!(req.url(), "/api/webhook/test-webhook-id");
                let auth = req.headers().iter().find(|h| {
                    let field_str = std::str::from_utf8(h.field.as_str().as_bytes()).unwrap_or("");
                    field_str.eq_ignore_ascii_case("authorization")
                });
                assert!(auth.is_some());
                assert_eq!(auth.unwrap().value.as_str(), "Bearer test-api-key");

                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                let json_body: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(json_body["action"], "update_boot_options");
                assert_eq!(json_body["mac"], "00:11:22:33:44:55");
                assert_eq!(json_body["boot_options"].as_array().unwrap()[0].as_str().unwrap(), "Ubuntu");

                let response = tiny_http::Response::from_string("{\"success\": true}")
                    .with_status_code(200);
                req.respond(response).unwrap();
            }
        });

        // Write state.json next to config.yaml
        let state_path = dir.path().join("state.json");
        let state_data = serde_json::json!({
            "paired": true,
            "token": "dummy-token",
            "webhook_id": "test-webhook-id",
            "api_key": "test-api-key",
            "ha_daemon_url": format!("http://127.0.0.1:{}", ha_port),
            "ha_grub_url": "http://127.0.0.1/grub",
        });
        std::fs::write(&state_path, serde_json::to_string_pretty(&state_data)?)?;

        // Run sync
        run_sync(&config_path, false)?;

        handle.join().unwrap();
        Ok(())
    }
}

