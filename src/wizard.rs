use anyhow::Result;
use cliclack::{confirm, input, intro, outro, select, spinner};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallMode {
    DaemonBoth,
    DaemonShutdownOnly,
    ShutdownHookOnly,
}

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

pub fn wizard_init(config_path: &Path) -> Result<bool> {
    check_write_permission(config_path).map_err(|e| {
        anyhow::anyhow!("Permission check failed: Cannot write to config file at {:?}. Error: {}", config_path, e)
    })?;

    intro("GrubStation Configuration Wizard")?;

    cliclack::clear_screen().unwrap();

    if config_path.exists() {
        let should_overwrite = confirm(format!(
            "GrubStation is already configured. Do you want to re-run setup and overwrite the existing configuration?",
        ))
        .initial_value(false)
        .interact()?;

        if !should_overwrite {
            outro("Initialization cancelled.")?;
            return Ok(false);
        }
    }

    let mut grub_config_path: Option<String> = None;
    if cfg!(target_os = "linux") {
        if let Some(path) = crate::config::DEFAULT_GRUB_PATHS
            .iter()
            .find(|p| Path::new(p).exists())
        {
            grub_config_path = Some((*path).to_string());
        }
    }

    let has_grub = grub_config_path.is_some();
    let mode = if has_grub {
        select("Installation Mode")
            .items(&[
                (
                    InstallMode::DaemonBoth,
                    "Daemon (Remote shutdown + Report boot options)",
                    "",
                ),
                (
                    InstallMode::DaemonShutdownOnly,
                    "Daemon (Remote shutdown only)",
                    "",
                ),
                (
                    InstallMode::ShutdownHookOnly,
                    "Shutdown hook (Report boot options only)",
                    "",
                ),
            ])
            .interact()?
    } else {
        select("Installation Mode")
            .items(&[(
                InstallMode::DaemonShutdownOnly,
                "Daemon (Remote shutdown only)",
                "",
            )])
            .interact()?
    };

    // Network interface selection
    let interfaces = NetworkInterface::show().map_err(|e| anyhow::anyhow!(e))?;
    let filtered_interfaces: Vec<_> = interfaces
        .into_iter()
        .filter(|itf| {
            // Filter out loopback
            if itf.name == "lo" || itf.name.starts_with("loop") {
                return false;
            }
            // Filter out virtual interfaces (common patterns)
            let virtual_patterns = ["veth", "docker", "br-", "virbr", "any", "tun", "tap"];
            if virtual_patterns.iter().any(|p| itf.name.starts_with(p)) {
                return false;
            }
            // Must have a MAC address and at least one IPv4 address
            itf.mac_addr.is_some() && itf.addr.iter().any(|a| a.ip().is_ipv4())
        })
        .collect();

    if filtered_interfaces.is_empty() {
        anyhow::bail!("No active ethernet interfaces found.");
    }

    let items: Vec<_> = filtered_interfaces
        .iter()
        .map(|itf| {
            let ips: Vec<_> = itf.addr.iter()
                .filter(|a| a.ip().is_ipv4())
                .map(|a| a.ip().to_string())
                .collect();
            let label = format!(
                "{} (MAC: {}, IPs: {})",
                itf.name,
                itf.mac_addr.as_ref().unwrap_or(&"Unknown".to_string()),
                ips.join(", ")
            );
            (itf, label)
        })
        .collect();

    let selected_itf = select("Available Network Interfaces:")
        .items(
            &items
                .iter()
                .map(|(itf, label)| (itf, label, ""))
                .collect::<Vec<_>>(),
        )
        .interact()?;

    // Ensure the selected interface has at least one IPv4 address
    let has_ipv4 = selected_itf.addr.iter().any(|addr| addr.ip().is_ipv4());
    if !has_ipv4 {
        anyhow::bail!("Selected interface '{}' has no IPv4 addresses", selected_itf.name);
    }


    // Daemon config
    let daemon = if mode == InstallMode::ShutdownHookOnly {
        None
    } else {
        let port_str: String = input("Daemon Port")
            .default_input(&crate::config::DEFAULT_DAEMON_PORT.to_string())
            .validate(|input: &String| {
                input
                    .parse::<u16>()
                    .map(|_| ())
                    .map_err(|_| "Invalid port number")
            })
            .interact()?;
        let port = port_str.parse::<u16>()?;
        Some(crate::config::DaemonConfig { port })
    };


    let mut network_wait = 2;
    if mode == InstallMode::DaemonBoth || mode == InstallMode::ShutdownHookOnly {
        let network_wait_str: String = input("GRUB Network Wait (seconds)")
            .default_input("2")
            .validate(|input: &String| {
                input
                    .parse::<u32>()
                    .map(|_| ())
                    .map_err(|_| "Invalid number of seconds")
            })
            .interact()?;
        network_wait = network_wait_str.parse::<u32>()?;
    }

    let grub = match mode {
        InstallMode::DaemonBoth | InstallMode::ShutdownHookOnly => {
            grub_config_path.map(|path| crate::config::GrubConfig {
                path: PathBuf::from(path),
                network_wait,
                webhook_id: String::new(),
            })
        }
        InstallMode::DaemonShutdownOnly => None,
    };

    let setup_pin = format!("{:06}", fastrand::u32(0..1_000_000));

    let config = crate::config::Config {
        host: crate::config::HostConfig { interface: selected_itf.name.clone() },
        daemon,
        grub,
        webhook_id: None,
        api_key: None,
        ha_daemon_url: None,
        ha_grub_url: None,
    };

    // Save config
    let yaml = serde_yaml::to_string(&config)?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, yaml)?;

    // Validate the generated config
    if let Err(e) = crate::config::load_config(config_path) {
        anyhow::bail!("Generated config failed validation: {}", e);
    }

    // Save initial state.json containing setup_pin and paired: false
    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
    let state_data = serde_json::json!({
        "paired": false,
        "setup_pin": setup_pin.clone(),
    });
    if let Ok(json_str) = serde_json::to_string_pretty(&state_data) {
        let _ = std::fs::write(&state_path, json_str);
    }

    cliclack::log::step(format!("Configuration saved to {:?}.",config_path))?;

    if mode != InstallMode::ShutdownHookOnly {
        cliclack::log::success(format!(
            "Home Assistant Pairing PIN: \x1b[1m{}\x1b[0m",
            setup_pin
        ))?;
    }

    if let Some(ref grub_config) = config.grub {
        if !grub_config.webhook_id.is_empty() {
            install_grub_hook(&config, grub_config, None, true)?;
        }
    }
    if mode == InstallMode::ShutdownHookOnly {
        let s = spinner();
        s.start("Registering grubstation shutdown hook...");
        crate::service::install_shutdown_hook(config_path)?;
        s.stop("GrubStation shutdown hook registered.");
    } else if config.daemon.is_some() {
        let s = spinner();
        s.start("Installing grubstation service...");
        crate::service::install_service(config_path)?;
        s.stop("GrubStation service installed.");

        let s = spinner();
        s.start("Starting grubstation service...");
        crate::service::start_service()?;
        s.stop("GrubStation service started.");
    }

    outro("GrubStation setup completed successfully!")?;

    Ok(false)
}

pub fn install_grub_hook_to_path(
    config: &crate::config::Config,
    grub_config: &crate::config::GrubConfig,
    hook_path: &Path,
    ha_grub_url: Option<&str>,
) -> Result<()> {
    let (mac, address) = crate::config::resolve_interface_details(&config.host.interface)?;
    let wait_list = (1..=grub_config.network_wait)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ");

    // GRUB uses a device-like path syntax for HTTP networking requests,
    // formatted as `(http,host)/path/to/resource`.
    let default_boot_url = format!("(http,{})/api/grubstation/{}?token={}", address, mac, grub_config.webhook_id);
    let boot_url = if let Some(url) = ha_grub_url {
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
    ha_grub_url: Option<&str>,
    run_update_grub: bool,
) -> Result<()> {
    let hook_path_str = std::env::var("GRUBSTATION_HOOK_PATH")
        .unwrap_or_else(|_| "/etc/grub.d/99_grubstation".to_string());
    let hook_path = Path::new(&hook_path_str);

    install_grub_hook_to_path(config, grub_config, hook_path, ha_grub_url)?;

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
            ha_daemon_url: None,
            ha_grub_url: None,
        };
        let grub_config = config.grub.as_ref().unwrap();

        assert!(install_grub_hook_to_path(&config, grub_config, &hook_path, None).is_ok());

        let (mac, address) = crate::config::resolve_interface_details("lo").unwrap();
        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(content.contains(&format!("set boot_url=\"(http,{})/api/grubstation/{}?token=test-webhook-123\"", address, mac)));
        assert!(content.contains("for i in 1 2 3; do"));
    }
}
