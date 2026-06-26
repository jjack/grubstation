use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

pub fn start_advertisement(config: &crate::config::Config, mac: &str, address: &str) -> Result<(ServiceDaemon, ServiceInfo)> {
    let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(crate::config::DEFAULT_DAEMON_PORT);

    let mdns = ServiceDaemon::new()?;
    let service_type = "_grubstation._tcp.local.";
    let instance_name = address.to_string();
    let system_hostname = hostname::get()?.to_string_lossy().into_owned();
    let host_name = format!("{}.local.", system_hostname);

    let mut properties = HashMap::new();
    properties.insert("mac".to_string(), mac.to_string());
    properties.insert("paired".to_string(), "false".to_string());
    properties.insert("address".to_string(), address.to_string());

    let service_info = ServiceInfo::new(
        service_type,
        &instance_name,
        &host_name,
        "", // Use automatic address detection
        port,
        Some(properties),
    )?.enable_addr_auto();

    mdns.register(service_info.clone())?;

    Ok((mdns, service_info))
}
