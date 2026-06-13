use anyhow::Result;
use cliclack::{confirm, input, intro, outro, select};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use std::path::{Path, PathBuf};

pub fn wizard_init(config_path: &Path) -> Result<()> {
    intro("Grubstation Configuration Wizard")?;

    if config_path.exists() {
        let should_overwrite = confirm(format!(
            "Config file already exists at {:?}. Overwrite?",
            config_path
        ))
        .initial_value(false)
        .interact()?;

        if !should_overwrite {
            outro("Initialization cancelled.")?;
            return Ok(());
        }
    }

    let mut found_grub_path: Option<String> = None;
    if cfg!(target_os = "linux") {
        let found = crate::config::DEFAULT_GRUB_PATHS
            .iter()
            .find(|p| Path::new(p).exists());

        if let Some(path) = found {
            let sync_grub = confirm(format!(
                "Found GRUB config at {}. Sync boot options with Home Assistant?",
                path
            ))
            .initial_value(true)
            .interact()?;

            if sync_grub {
                found_grub_path = Some((*path).to_string());
            }
        }
    }

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

    let selected_itf = select("Select the network interface to use:")
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
        address_options.push((ip.clone(), format!("IP: {}", ip), ""));
    }

    // Add Hostname
    if let Ok(h) = hostname::get() {
        let h_str = h.to_string_lossy().into_owned();
        address_options.push((h_str.clone(), format!("Hostname: {}", h_str), ""));

        // Try to get FQDN
        if let Ok(mut addrs) = dns_lookup::getaddrinfo(Some(&h_str), None, None) {
            if let Some(Ok(info)) = addrs.next() {
                if let Some(canon) = info.canonname {
                    if canon != h_str {
                        address_options.push((canon.clone(), format!("FQDN: {}", canon), ""));
                    }
                }
            }
        }
    }

    let ip = if address_options.len() == 1 {
        address_options[0].0.clone()
    } else {
        select("Select the address to use for this host:")
            .items(&address_options)
            .interact()?
    };

    // Daemon config
    let setup_daemon = confirm("Do you want to run the daemon?")
        .initial_value(true)
        .interact()?;
    let daemon = if setup_daemon {
        let port_str: String = input("Enter the daemon port:")
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
    } else {
        None
    };

    // WoL config
    let wake_on_lan = {
        let broadcast_address: String = input("Enter the broadcast address:")
            .default_input(crate::config::DEFAULT_BROADCAST_ADDRESS)
            .interact()?;
        let port_str: String = input("Enter the broadcast port:")
            .default_input(&crate::config::DEFAULT_BROADCAST_PORT.to_string())
            .validate(|input: &String| {
                input
                    .parse::<u16>()
                    .map(|_| ())
                    .map_err(|_| "Invalid port number")
            })
            .interact()?;
        let broadcast_port = port_str.parse::<u16>()?;
        Some(crate::config::WakeOnLanConfig {
            broadcast_address,
            broadcast_port,
        })
    };

    let grub = found_grub_path.map(|path| crate::config::GrubConfig {
        path: PathBuf::from(path),
    });

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
    Ok(())
}
