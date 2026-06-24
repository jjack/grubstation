use anyhow::Result;
use cliclack::{confirm, input, intro, outro, select};
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

    intro("Grubstation Configuration Wizard")?;

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
            // Must have a MAC address and at least one IP
            itf.mac_addr.is_some() && !itf.addr.is_empty()
        })
        .collect();

    if filtered_interfaces.is_empty() {
        anyhow::bail!("No active ethernet interfaces found.");
    }

    let items: Vec<_> = filtered_interfaces
        .iter()
        .map(|itf| {
            let ips: Vec<_> = itf.addr.iter().map(|a| a.ip().to_string()).collect();
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

    let mac = selected_itf.mac_addr.as_ref().unwrap().clone();

    // Select host address (IP, Hostname, or FQDN)
    let mut address_options = Vec::new();

    // Add IP addresses
    for addr in &selected_itf.addr {
        let ip = addr.ip().to_string();
        address_options.push((ip.clone(), format!("{} (IP Address - Ensure this is static!)", ip), ""));
    }

    // Add Hostname
    if let Ok(h) = hostname::get() {
        let h_str = h.to_string_lossy().into_owned();
        address_options.push((h_str.clone(), format!("{} (Hostname)", h_str), ""));

        // Try to get FQDN
        if let Ok(mut addrs) = dns_lookup::getaddrinfo(Some(&h_str), None, None) {
            if let Some(Ok(info)) = addrs.next() {
                if let Some(canon) = info.canonname {
                    if canon != h_str {
                        address_options.push((canon.clone(), format!("{} (FQDN)", canon), ""));
                    }
                }
            }
        }
    }

    let ip = if address_options.len() == 1 {
        address_options[0].0.clone()
    } else {
        select("Host Address (Used for communication with the daemon)")
            .items(&address_options)
            .interact()?
    };

    let mut broadcast_ip: Option<String> = None;
    let matched_addr = selected_itf
        .addr
        .iter()
        .find(|addr| addr.ip().to_string() == ip)
        .or_else(|| {
            selected_itf
                .addr
                .iter()
                .find(|addr| matches!(addr, network_interface::Addr::V4(_)))
        });

    if let Some(network_interface::Addr::V4(v4_addr)) = matched_addr {
        if let Some(bcast) = v4_addr.broadcast {
            broadcast_ip = Some(bcast.to_string());
        } else if let Some(netmask) = v4_addr.netmask {
            let ip_octets = v4_addr.ip.octets();
            let mask_octets = netmask.octets();
            let mut bcast_octets = [0u8; 4];
            for i in 0..4 {
                bcast_octets[i] = ip_octets[i] | !mask_octets[i];
            }
            broadcast_ip = Some(std::net::Ipv4Addr::from(bcast_octets).to_string());
        }
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

    // WoL config
    let wake_on_lan = {
        let global_bcast = crate::config::DEFAULT_BROADCAST_ADDRESS;
        let mut options = vec![
            (
                global_bcast.to_string(),
                format!("Global Broadcast ({})", global_bcast),
                "",
            ),
        ];
        if let Some(ref subnet_bcast) = broadcast_ip {
            if subnet_bcast != global_bcast {
                options.push((
                    subnet_bcast.clone(),
                    format!("Subnet Broadcast ({})", subnet_bcast),
                    "",
                ));
            }
        }

        let broadcast_address = select("WOL Broadcast Address")
            .items(&options)
            .interact()?;
        let broadcast_port: u16 = crate::config::DEFAULT_BROADCAST_PORT;

        Some(crate::config::WakeOnLanConfig {
            broadcast_address,
            broadcast_port,
        })
    };

    let mut network_wait = 2;
    let mut webhook_id = String::new();
    if mode == InstallMode::DaemonBoth || mode == InstallMode::ShutdownHookOnly {
        webhook_id = input("Home Assistant Webhook ID")
            .validate(|input: &String| {
                if input.trim().is_empty() {
                    Err("Webhook ID cannot be empty")
                } else {
                    Ok(())
                }
            })
            .interact()?;

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
                webhook_id,
            })
        }
        InstallMode::DaemonShutdownOnly => None,
    };

    let config = crate::config::Config {
        host: crate::config::HostConfig { mac, address: ip },
        daemon,
        wake_on_lan,
        grub,
    };

    // Save config
    let yaml = serde_yaml::to_string(&config)?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, yaml)?;

    outro(format!(
        "Configuration saved to {:?}. Success!",
        config_path
    ))?;

    if let Some(ref grub_config) = config.grub {
        install_grub_hook(&config, grub_config)?;
    }

    if config.daemon.is_some() {
        crate::service::install_and_start_service(config_path)?;
    }

    Ok(false)
}

fn install_grub_hook_to_path(
    config: &crate::config::Config,
    grub_config: &crate::config::GrubConfig,
    hook_path: &Path,
) -> Result<()> {
    let wait_list = (1..=grub_config.network_wait)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ");

    let hook_content = include_str!("../templates/99_grubstation")
        .replace("{{HOST}}", &config.host.address)
        .replace("{{MAC_ADDRESS}}", &config.host.mac)
        .replace("{{WEBHOOK_ID}}", &grub_config.webhook_id)
        .replace("{{WAIT_TIME_SECONDS}}", &grub_config.network_wait.to_string())
        .replace("{{WAIT_LIST}}", &wait_list);

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

    println!("note: the exact GRUB networking configuration applied by this tool may not work perfectly for every motherboard due to how finicky UEFI and network firmware can be across different hardware vendors. If your system struggles to connect to the network from within GRUB, you may need to manually troubleshoot your GRUB network settings.");

    Ok(())
}

fn install_grub_hook(config: &crate::config::Config, grub_config: &crate::config::GrubConfig) -> Result<()> {
    install_grub_hook_to_path(config, grub_config, Path::new("/etc/grub.d/99_grubstation"))
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
                mac: "00:11:22:33:44:55".to_string(),
                address: "192.168.1.100".to_string(),
            },
            daemon: None,
            wake_on_lan: None,
            grub: Some(crate::config::GrubConfig {
                path: PathBuf::from("Cargo.toml"),
                network_wait: 3,
                webhook_id: "test-webhook-123".to_string(),
            }),
        };
        let grub_config = config.grub.as_ref().unwrap();

        assert!(install_grub_hook_to_path(&config, grub_config, &hook_path).is_ok());

        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(content.contains("set boot_url=\"(http,192.168.1.100)/api/grubstation/00:11:22:33:44:55?token=test-webhook-123\""));
        assert!(content.contains("for i in 1 2 3; do"));
    }
}

