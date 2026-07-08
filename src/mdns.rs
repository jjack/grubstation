use anyhow::Result;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

pub fn start_advertisement(config: &crate::config::Config, _mac: &str, address: &str) -> Result<(ServiceDaemon, ServiceInfo)> {
    let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(crate::config::DEFAULT_DAEMON_PORT);

    let mdns = ServiceDaemon::new()?;
    mdns.disable_interface(IfKind::IPv6)?;

    let service_type = "_grubstation._tcp.local.";
    let instance_name = address.to_string();
    let system_hostname = hostname::get()?.to_string_lossy().into_owned();
    let host_name = format!("{}.local.", system_hostname);

    let mut properties = HashMap::new();
    properties.insert("paired".to_string(), "false".to_string());

    let service_info = ServiceInfo::new(
        service_type,
        &instance_name,
        &host_name,
        address,
        port,
        Some(properties),
    )?.enable_addr_auto();

    mdns.register(service_info.clone())?;

    Ok((mdns, service_info))
}
