mod config;
mod grub;
mod wizard;
mod service;
mod mdns;
mod server;
mod client;

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
    /// Pairs the host machine with Home Assistant.
    Pair {
        /// Optional pairing JSON payload. If omitted, starts a temporary pairing HTTP server.
        #[arg(long)]
        payload: Option<String>,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
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
            server::start_server(&config, config_path.clone(), mdns, service_info, false)?;
            println!("Grubstation daemon is running and advertising service via mDNS on port {}...", port);

            let (tx, rx) = std::sync::mpsc::channel();
            ctrlc::set_handler(move || {
                let _ = tx.send(());
            })?;

            println!("Press Ctrl+C to stop...");
            let _ = rx.recv();
            println!("Shutting down daemon...");
        }
        Commands::Pair { payload } => {
            if let Some(json_str) = payload {
                // Parse manual payload
                let pair_req: crate::server::PairRequest = serde_json::from_str(json_str)
                    .map_err(|e| anyhow::anyhow!("Invalid JSON payload: {}", e))?;

                println!("Manual pairing payload detected. Parsing...");

                // Parse GRUB entries
                let config = config::load_config(&config_path).map_err(|e| anyhow::anyhow!("{}", e))?;
                let entries = if let Some(ref gc) = config.grub {
                    crate::grub::parse_grub_entries(&gc.path)?
                } else {
                    if let Some(path) = crate::config::DEFAULT_GRUB_PATHS.iter().map(std::path::PathBuf::from).find(|p| p.exists()) {
                        crate::grub::parse_grub_entries(&path)?
                    } else {
                        anyhow::bail!("No GRUB configuration file found at default paths.")
                    }
                };

                // Push initial boot options
                println!("Pushing initial boot options to Home Assistant...");
                crate::client::push_boot_options(
                    &pair_req.ha_daemon_url,
                    &pair_req.webhook_id,
                    &pair_req.api_key,
                    &config.host.mac,
                    &entries,
                )?;

                // If apply_config is true, install/apply the GRUB boot hook
                if pair_req.apply_config {
                    println!("Applying GRUB configuration...");
                    let mut grub_config = if let Some(ref gc) = config.grub {
                        gc.clone()
                    } else {
                        let path = crate::config::DEFAULT_GRUB_PATHS
                            .iter()
                            .map(std::path::PathBuf::from)
                            .find(|p| p.exists())
                            .unwrap_or_else(|| std::path::PathBuf::from("/boot/grub/grub.cfg"));
                        crate::config::GrubConfig {
                            path,
                            network_wait: 10,
                            webhook_id: pair_req.webhook_id.clone(),
                        }
                    };
                    grub_config.webhook_id = pair_req.webhook_id.clone();
                    crate::wizard::install_grub_hook(&config, &grub_config, Some(&pair_req.ha_grub_url))?;
                }

                // Generate a random token
                let token = std::iter::repeat_with(fastrand::alphanumeric)
                    .take(32)
                    .collect::<String>();

                // Save pairing state and request data to state.json
                let state_path = config_path.parent().unwrap_or(std::path::Path::new(".")).join("state.json");
                let state_file_data = serde_json::json!({
                    "paired": true,
                    "token": token,
                    "webhook_id": pair_req.webhook_id,
                    "api_key": pair_req.api_key,
                    "ha_daemon_url": pair_req.ha_daemon_url,
                    "ha_grub_url": pair_req.ha_grub_url,
                });

                let json_str = serde_json::to_string_pretty(&state_file_data)?;
                log::info!("Saving manual pairing state to {:?}", state_path);
                std::fs::write(&state_path, json_str)?;

                println!("Manual pairing successful! Token saved to state.json");
            } else {
                println!("Starting temporary pairing server...");
                let config = config::load_config(&config_path).map_err(|e| anyhow::anyhow!("{}", e))?;
                let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(config::DEFAULT_DAEMON_PORT);

                let (mdns, service_info) = mdns::start_advertisement(&config)?;
                server::start_server(&config, config_path.clone(), mdns, service_info, true)?;
                
                println!("Temporary pairing server is running and advertising via mDNS on port {}...", port);
                println!("This server will automatically exit after successful pairing or in 5 minutes.");

                // Spawn timeout thread
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(300));
                    println!("\nPairing timeout reached. Exiting...");
                    std::process::exit(1);
                });

                // Keep main thread alive (or wait for ctrl-c)
                let (tx, rx) = std::sync::mpsc::channel();
                ctrlc::set_handler(move || {
                    let _ = tx.send(());
                })?;
                let _ = rx.recv();
                println!("Shutdown signal received. Exiting...");
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

        let payload = crate::client::build_boot_options_payload(&config.host.mac, &entries);

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
            if let Err(err) = crate::client::push_boot_options(
                ha_daemon_url,
                webhook_id,
                api_key,
                &config.host.mac,
                &entries,
            ) {
                if err.to_string().contains("Webhook unregistered") {
                    eprintln!("Home Assistant indicates the webhook is unregistered/deleted. Resetting pairing state...");
                    let _ = std::fs::remove_file(&state_path);
                }
                return Err(err);
            }
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

    #[test]
    fn test_pair_subcommand_manual() -> Result<()> {
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

                let response = tiny_http::Response::from_string("{\"success\": true}")
                    .with_status_code(200);
                req.respond(response).unwrap();
            }
        });

        // Run pair command via subcommand matching emulation
        let payload_str = format!(
            "{{\"ha_daemon_url\":\"http://127.0.0.1:{}\",\"webhook_id\":\"test-webhook-id\",\"api_key\":\"test-api-key\",\"ha_grub_url\":\"http://127.0.0.1/grub\",\"apply_config\":false}}",
            ha_port
        );

        let cli_cmd = Commands::Pair {
            payload: Some(payload_str),
        };

        // Emulate match command execution block for Commands::Pair
        match cli_cmd {
            Commands::Pair { payload } => {
                let payload_val = payload.unwrap();
                let pair_req: crate::server::PairRequest = serde_json::from_str(&payload_val)?;
                let config = config::load_config(&config_path).map_err(|e| anyhow::anyhow!("{}", e))?;
                let entries = crate::grub::parse_grub_entries(&config.grub.as_ref().unwrap().path)?;
                
                crate::client::push_boot_options(
                    &pair_req.ha_daemon_url,
                    &pair_req.webhook_id,
                    &pair_req.api_key,
                    &config.host.mac,
                    &entries,
                )?;
                
                let token = "test-token";
                let state_path = config_path.parent().unwrap().join("state.json");
                let state_file_data = serde_json::json!({
                    "paired": true,
                    "token": token,
                    "webhook_id": pair_req.webhook_id,
                    "api_key": pair_req.api_key,
                    "ha_daemon_url": pair_req.ha_daemon_url,
                    "ha_grub_url": pair_req.ha_grub_url,
                });
                std::fs::write(&state_path, serde_json::to_string_pretty(&state_file_data)?)?;
            }
            _ => unreachable!(),
        }

        // Verify state.json was written correctly
        let state_path = dir.path().join("state.json");
        assert!(state_path.exists());
        let state_content = std::fs::read_to_string(&state_path)?;
        let state_json: serde_json::Value = serde_json::from_str(&state_content)?;
        assert!(state_json["paired"].as_bool().unwrap());
        assert_eq!(state_json["webhook_id"].as_str().unwrap(), "test-webhook-id");

        handle.join().unwrap();
        Ok(())
    }
}

