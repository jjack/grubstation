use anyhow::Result;
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn check_write_permission(path: &Path) -> Result<()> {
    if path.exists() {
        if path.is_dir() {
            anyhow::bail!("Path is a directory: {:?}", path);
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)?;
    } else {
        let mut ancestor = path.parent().unwrap_or_else(|| Path::new("."));
        if ancestor.as_os_str().is_empty() {
            ancestor = Path::new(".");
        }
        while !ancestor.exists() {
            if let Some(parent) = ancestor.parent() {
                ancestor = parent;
                if ancestor.as_os_str().is_empty() {
                    ancestor = Path::new(".");
                    break;
                }
            } else {
                break;
            }
        }
        let test_file = ancestor.join(".grubstation_write_test");
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(&test_file)?;
        std::fs::remove_file(&test_file)?;
    }
    Ok(())
}

pub fn get_json_dump(config: &crate::config::Config) -> Result<serde_json::Value> {
    let (mac, address) = crate::config::resolve_interface_details(&config.host.interface)?;
    let system_hostname = hostname::get()?.to_string_lossy().into_owned();
    let os_name = crate::server::get_os_name();
    
    // Parse GRUB entries
    let entries = if let Some(ref gc) = config.grub {
        crate::grub::parse_grub_entries(&gc.path)?
    } else {
        if let Some(path) = crate::config::DEFAULT_GRUB_PATHS.iter().map(std::path::PathBuf::from).find(|p| p.exists()) {
            crate::grub::parse_grub_entries(&path)?
        } else {
            Vec::new()
        }
    };

    Ok(serde_json::json!({
        "mac": mac,
        "address": address,
        "hostname": system_hostname,
        "os": os_name,
        "boot_options": entries,
    }))
}

pub fn wizard_init(config_path: &Path, daemonless: bool) -> Result<bool> {
    check_write_permission(config_path).map_err(|e| {
        anyhow::anyhow!("Permission check failed: Cannot write to config file at {:?}. Error: {}", config_path, e)
    })?;

    println!("Initializing GrubStation configuration...");

    let config = if config_path.exists() {
        println!("Existing configuration found at {:?}", config_path);
        let mut loaded = match crate::config::load_config(config_path) {
            Ok(c) => c,
            Err(e) => {
                println!("Warning: Failed to load existing config: {}. Recreating...", e);
                create_default_config(daemonless)?
            }
        };

        // Update daemon config based on daemonless flag
        if daemonless {
            loaded.daemon = None;
        } else if loaded.daemon.is_none() {
            loaded.daemon = Some(crate::config::DaemonConfig {
                port: crate::config::DEFAULT_DAEMON_PORT,
            });
        }

        let yaml = serde_yaml::to_string(&loaded)?;
        std::fs::write(config_path, yaml)?;
        loaded
    } else {
        let created = create_default_config(daemonless)?;
        let yaml = serde_yaml::to_string(&created)?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(config_path, yaml)?;
        println!("New configuration generated and saved to {:?}", config_path);
        created
    };

    // Validate the generated config
    if let Err(e) = crate::config::load_config(config_path) {
        anyhow::bail!("Generated config failed validation: {}", e);
    }

    let setup_pin = format!("{:06}", fastrand::u32(0..1_000_000));

    // Save initial state.json containing setup_pin and paired: false
    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
    let state_data = serde_json::json!({
        "paired": false,
        "setup_pin": setup_pin.clone(),
    });
    let json_str = serde_json::to_string_pretty(&state_data)?;
    std::fs::write(&state_path, json_str)?;

    if daemonless {
        println!("Installing shutdown hook...");
        if let Err(e) = crate::service::install_shutdown_hook(config_path) {
            println!("Warning: Failed to install shutdown hook (perhaps running without root?): {}", e);
        } else {
            println!("GrubStation shutdown hook registered successfully.");
        }

        println!("\nGrubStation initialized in daemonless/shutdown-only mode!");
        println!("Your setup PIN is: \x1b[1;32m{}\x1b[0m", setup_pin);
        
        let dump = get_json_dump(&config)?;
        println!("\nJSON Dump for manual configuration in Home Assistant:");
        println!("{}", serde_json::to_string_pretty(&dump)?);
    } else {
        println!("\nYour setup PIN is: \x1b[1;32m{}\x1b[0m", setup_pin);
        println!("Starting temporary pairing server in-process...");

        let (mac, address) = crate::config::resolve_interface_details(&config.host.interface)?;
        let (mdns, service_info) = crate::mdns::start_advertisement(&config, &mac, &address)?;

        let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(crate::config::DEFAULT_DAEMON_PORT);
        println!("Temporary pairing server is running and advertising via mDNS on port {}...", port);
        println!("Enter the PIN in Home Assistant to complete configuration.");
        println!("This server will automatically exit after successful pairing or in 5 minutes.");

        crate::server::start_server(&config, config_path.to_path_buf(), mdns, service_info, mac, address, true)?;

        // Poll state.json until paired or timeout (5 minutes)
        let start_time = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(300);
        let mut paired = false;

        // Listen for Ctrl+C to cancel setup
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c_cancelled = Arc::clone(&cancelled);
        ctrlc::set_handler(move || {
            c_cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        })?;

        println!("Waiting for pairing... Press Ctrl+C to cancel.");
        while start_time.elapsed() < timeout {
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("Pairing cancelled by user.");
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            if state_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&state_path) {
                    if let Ok(state_val) = serde_json::from_str::<serde_json::Value>(&content) {
                        if state_val["paired"].as_bool().unwrap_or(false) {
                            paired = true;
                            break;
                        }
                    }
                }
            }
        }

        if !paired {
            anyhow::bail!("Pairing timeout reached or pairing failed. Please try running `grubstation init` again.");
        }

        println!("\nPairing successful!");
        println!("Waiting 3 seconds for initial sync to complete...");
        std::thread::sleep(std::time::Duration::from_secs(3));

        println!("Installing system service...");
        if let Err(e) = crate::service::install_service(config_path) {
            println!("Warning: Failed to install system service: {}. You may need to run this command with sudo/Administrator privileges.", e);
        } else {
            println!("GrubStation system service installed.");
            println!("Starting system service...");
            if let Err(e) = crate::service::start_service() {
                println!("Warning: Failed to start system service: {}. You may need to run this command with sudo/Administrator privileges.", e);
            } else {
                println!("Setup complete! Daemon registered and running in the background.");
            }
        }
    }

    Ok(true)
}

fn create_default_config(daemonless: bool) -> Result<crate::config::Config> {
    let interfaces = NetworkInterface::show().map_err(|e| anyhow::anyhow!(e))?;
    let filtered_interfaces: Vec<_> = interfaces
        .into_iter()
        .filter(|itf| {
            if itf.name == "lo" || itf.name.starts_with("loop") {
                return false;
            }
            let virtual_patterns = ["veth", "docker", "br-", "virbr", "any", "tun", "tap"];
            if virtual_patterns.iter().any(|p| itf.name.starts_with(p)) {
                return false;
            }
            itf.mac_addr.is_some() && itf.addr.iter().any(|a| a.ip().is_ipv4())
        })
        .collect();

    if filtered_interfaces.is_empty() {
        anyhow::bail!("No active ethernet interfaces found.");
    }

    let selected_itf = &filtered_interfaces[0];

    let mut grub_config_path: Option<String> = None;
    if cfg!(target_os = "linux") {
        if let Some(path) = crate::config::DEFAULT_GRUB_PATHS
            .iter()
            .find(|p| Path::new(p).exists())
        {
            grub_config_path = Some((*path).to_string());
        }
    }

    let grub = if let Some(path) = grub_config_path {
        Some(crate::config::GrubConfig {
            path: PathBuf::from(path),
            network_wait: 10,
            webhook_id: String::new(),
        })
    } else {
        None
    };

    let daemon = if daemonless {
        None
    } else {
        Some(crate::config::DaemonConfig {
            port: crate::config::DEFAULT_DAEMON_PORT,
        })
    };

    Ok(crate::config::Config {
        host: crate::config::HostConfig {
            interface: selected_itf.name.clone(),
        },
        daemon,
        grub,
        webhook_id: None,
        api_key: None,
        ha_url: None,
    })
}

pub fn install_grub_hook_to_path(
    config: &crate::config::Config,
    grub_config: &crate::config::GrubConfig,
    hook_path: &Path,
    grub_boot_url: Option<&str>,
) -> Result<()> {
    let (mac, address) = crate::config::resolve_interface_details(&config.host.interface)?;
    let wait_list = (1..=grub_config.network_wait)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ");

    // GRUB uses a device-like path syntax for HTTP networking requests,
    // formatted as `(http,host)/path/to/resource`.
    let default_boot_url = format!("(http,{})/api/grubstation/{}?token={}", address, mac, grub_config.webhook_id);
    let boot_url = if let Some(url) = grub_boot_url {
        // Strip the protocol prefix (e.g., http:// or https://)
        let url_without_proto = url.trim_start_matches("http://").trim_start_matches("https://");
        
        // Split into host (and port) vs. the request path at the first '/'
        if let Some(pos) = url_without_proto.find('/') {
            let host = &url_without_proto[..pos];
            let path = &url_without_proto[pos..];
            format!("(http,{}){}", host, path)
        } else {
            format!("(http,{})", url_without_proto)
        }
    } else {
        default_boot_url
    };

    let hook_content = include_str!("../templates/99_grubstation")
        .replace("(http,{{HOST}})/api/grubstation/{{MAC_ADDRESS}}?token={{WEBHOOK_ID}}", &boot_url)
        .replace("{{WAIT_TIME_SECONDS}}", &grub_config.network_wait.to_string())
        .replace("{{WAIT_LIST}}", &wait_list);

    log::info!("Updating GRUB config hook at {:?}", hook_path);
    if let Some(parent) = hook_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(hook_path, hook_content)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(hook_path, perms)?;
    }

    log::info!("Successfully updated GRUB config hook.");
    println!("note: the exact GRUB networking configuration applied by this tool may not work perfectly for every motherboard due to how finicky UEFI and network firmware can be across different hardware vendors. If your system struggles to connect to the network from within GRUB, you may need to manually troubleshoot your GRUB network settings.");

    Ok(())
}

pub fn install_grub_hook(
    config: &crate::config::Config,
    grub_config: &crate::config::GrubConfig,
    grub_boot_url: Option<&str>,
    run_update_grub: bool,
) -> Result<()> {
    let hook_path_str = std::env::var("GRUBSTATION_HOOK_PATH")
        .unwrap_or_else(|_| "/etc/grub.d/99_grubstation".to_string());
    let hook_path = Path::new(&hook_path_str);

    install_grub_hook_to_path(config, grub_config, hook_path, grub_boot_url)?;

    if run_update_grub {
        #[cfg(target_os = "linux")]
        {
        log::info!("Running update-grub to apply GRUB configuration changes...");
        
        let run_cmd = |cmd: &str, args: &[&str]| -> std::io::Result<std::process::ExitStatus> {
            std::process::Command::new(cmd).args(args).status()
        };

        // Try update-grub first
        let result = run_cmd("update-grub", &[]);
        
        let status = match result {
            Ok(status) => Some(status),
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Try grub-mkconfig
                let mkconfig_path = grub_config.path.to_string_lossy();
                match run_cmd("grub-mkconfig", &["-o", &mkconfig_path]) {
                    Ok(status) => Some(status),
                    Err(ref e2) if e2.kind() == std::io::ErrorKind::NotFound => {
                        // Try grub2-mkconfig
                        match run_cmd("grub2-mkconfig", &["-o", &mkconfig_path]) {
                            Ok(status) => Some(status),
                            Err(ref e3) if e3.kind() == std::io::ErrorKind::NotFound => {
                                None
                            }
                            Err(e3) => return Err(anyhow::anyhow!("Failed to run grub2-mkconfig: {}", e3)),
                        }
                    }
                    Err(e2) => return Err(anyhow::anyhow!("Failed to run grub-mkconfig: {}", e2)),
                }
            }
            Err(e) => return Err(anyhow::anyhow!("Failed to run update-grub: {}", e)),
        };

        match status {
            Some(status) if status.success() => {
                log::info!("Successfully ran update-grub / grub-mkconfig.");
            }
            Some(status) => {
                log::warn!("update-grub / grub-mkconfig failed with exit status: {}. Please run it manually.", status);
            }
            None => {
                log::warn!("Neither update-grub nor grub-mkconfig/grub2-mkconfig was found in PATH. Skipping GRUB configuration update.");
            }
        }
    }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::File;

    #[test]
    fn test_check_write_permission_existing_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config.yaml");
        File::create(&file_path).unwrap();
        assert!(check_write_permission(&file_path).is_ok());
    }

    #[test]
    fn test_check_write_permission_non_existing_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config.yaml");
        assert!(check_write_permission(&file_path).is_ok());
    }

    #[test]
    fn test_check_write_permission_nested_non_existing_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("nested/dir/config.yaml");
        assert!(check_write_permission(&file_path).is_ok());
    }

    #[test]
    fn test_check_write_permission_is_directory() {
        let dir = tempdir().unwrap();
        assert!(check_write_permission(dir.path()).is_err());
    }

    #[test]
    fn test_check_write_permission_read_only() {
        #[cfg(unix)]
        {
            // If running as root, the OS bypasses read-only permissions, so skip this test.
            if let Ok(output) = std::process::Command::new("id").arg("-u").output() {
                if let Ok(uid_str) = String::from_utf8(output.stdout) {
                    if uid_str.trim() == "0" {
                        return;
                    }
                }
            }
        }

        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config.yaml");
        {
            let file = File::create(&file_path).unwrap();
            let mut perms = file.metadata().unwrap().permissions();
            perms.set_readonly(true);
            file.set_permissions(perms).unwrap();
        }
        assert!(check_write_permission(&file_path).is_err());
    }

    #[test]
    fn test_install_grub_hook_to_path() {
        let dir = tempdir().unwrap();
        let hook_path = dir.path().join("99_grubstation");
        let config = crate::config::Config {
            host: crate::config::HostConfig {
                interface: "lo".to_string(),
            },
            daemon: None,
            grub: Some(crate::config::GrubConfig {
                path: PathBuf::from("Cargo.toml"),
                network_wait: 3,
                webhook_id: "test-webhook-123".to_string(),
            }),
            webhook_id: None,
            api_key: None,
            ha_url: None,
        };
        let grub_config = config.grub.as_ref().unwrap();

        assert!(install_grub_hook_to_path(&config, grub_config, &hook_path, None).is_ok());

        let (mac, address) = crate::config::resolve_interface_details("lo").unwrap();
        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(content.contains(&format!("set boot_url=\"(http,{})/api/grubstation/{}?token=test-webhook-123\"", address, mac)));
        assert!(content.contains("for i in 1 2 3; do"));
    }
}
