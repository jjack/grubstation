use anyhow::Result;
use http::StatusCode;
use std::sync::{Arc, Mutex};
use std::thread;
use log::{error, warn, info, debug};
use tiny_http::{Server, Response, Request, Header};
use serde_json::json;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};

#[derive(Debug, Serialize, Deserialize)]
pub struct PairRequest {
    pub webhook_id: String,
    pub api_key: String,
    pub ha_url: String,
    pub update_grub: bool,
    pub interface: String,
}

#[derive(Debug, Deserialize)]
struct UpdateRequest {
    ha_url: Option<String>,
    #[serde(default)]
    update_grub: bool,
}

struct DaemonState {
    paired: bool,
    token: Option<String>,
    setup_pin: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    paired: bool,
    token: Option<String>,
    setup_pin: Option<String>,
    webhook_id: Option<String>,
    api_key: Option<String>,
    ha_url: Option<String>,
}

pub fn start_server(
    config: &crate::config::Config,
    config_path: PathBuf,
    mdns: ServiceDaemon,
    service_info: ServiceInfo,
    mac: String,
    address: String,
    is_temp: bool,
) -> Result<()> {
    let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(crate::config::DEFAULT_DAEMON_PORT);
    let server = Server::http(format!("0.0.0.0:{}", port))
        .map_err(|e| anyhow::anyhow!("Failed to start HTTP server: {}", e))?;

    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
    
    let mut initial_paired = false;
    let mut initial_token = None;
    let mut initial_setup_pin = None;
    let mut webhook_id = None;
    let mut api_key = None;
    let mut ha_url = None;

    if state_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&state_path) {
            if let Ok(state) = serde_json::from_str::<PersistedState>(&content) {
                initial_paired = state.paired;
                initial_token = state.token;
                initial_setup_pin = state.setup_pin;
                webhook_id = state.webhook_id;
                api_key = state.api_key;
                ha_url = state.ha_url;
            }
        }
    }

    let state = Arc::new(Mutex::new(DaemonState {
        paired: initial_paired,
        token: initial_token,
        setup_pin: initial_setup_pin,
    }));

    let mdns = Arc::new(mdns);
    let current_service_info = Arc::new(Mutex::new(service_info));
    let config = config.clone();

    // If loaded state is already paired, update the mDNS advertisement and trigger startup sync
    if initial_paired {
        let mut info = current_service_info.lock().unwrap();
        let fullname = info.get_fullname().to_string();
        let _ = mdns.unregister(&fullname);

        let mut properties = std::collections::HashMap::new();
        properties.insert("paired".to_string(), "true".to_string());

        if let Ok(new_info) = mdns_sd::ServiceInfo::new(
            "_grubstation._tcp.local.",
            &address,
            info.get_hostname(),
            "",
            info.get_port(),
            Some(properties),
        ) {
            let new_info = new_info.enable_addr_auto();
            if let Ok(()) = mdns.register(new_info.clone()) {
                *info = new_info;
            }
        }

        // Trigger startup sync of boot options to Home Assistant
        if let (Some(webhook_id), Some(ha_url)) = (webhook_id, ha_url) {
            let api_key = api_key.unwrap_or_default();
            let sync_mac = mac.clone();
            let config = config.clone();
            let state = Arc::clone(&state);
            let mdns = Arc::clone(&mdns);
            let current_service_info = Arc::clone(&current_service_info);
            let shutdown_address = address.clone();
            let state_path = state_path.clone();
            
            std::thread::spawn(move || {
                info!("Daemon started in paired state. Performing startup sync of boot options to HA...");
                
                // Parse the host's current GRUB entries
                let entries_res = if let Some(ref gc) = config.grub {
                    crate::grub::parse_grub_entries(&gc.path)
                } else {
                    if let Some(path) = crate::config::DEFAULT_GRUB_PATHS.iter().map(PathBuf::from).find(|p| p.exists()) {
                        crate::grub::parse_grub_entries(&path)
                    } else {
                        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "No GRUB config found"))
                    }
                };

                match entries_res {
                    Ok(entries) => {
                        match crate::client::push_boot_options(
                            &ha_url,
                            &webhook_id,
                            &api_key,
                            &sync_mac,
                            &entries,
                        ) {
                            Ok(()) => {
                                info!("Startup sync of boot options completed successfully!");
                            }
                            Err(err) => {
                                error!("Startup sync of boot options failed: {}", err);
                                if err.to_string().contains("Webhook unregistered") {
                                    warn!("Home Assistant indicates the webhook is unregistered/deleted. Resetting local pairing state to unpaired...");
                                    
                                    // Reset internal state
                                    {
                                        let mut s = state.lock().unwrap();
                                        s.paired = false;
                                        s.token = None;
                                    }
                                    
                                    // Delete state.json
                                    let _ = std::fs::remove_file(&state_path);
                                    
                                    // Update mDNS advertisement to paired=false
                                    let mut info = current_service_info.lock().unwrap();
                                    let fullname = info.get_fullname().to_string();
                                    let _ = mdns.unregister(&fullname);
                                    
                                    let mut properties = std::collections::HashMap::new();
                                    properties.insert("paired".to_string(), "false".to_string());
                                    
                                    if let Ok(new_info) = mdns_sd::ServiceInfo::new(
                                        "_grubstation._tcp.local.",
                                        &shutdown_address,
                                        info.get_hostname(),
                                        "",
                                        info.get_port(),
                                        Some(properties),
                                    ) {
                                        let new_info = new_info.enable_addr_auto();
                                        if let Ok(()) = mdns.register(new_info.clone()) {
                                            *info = new_info;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        error!("Failed to parse GRUB entries for startup sync: {}", err);
                    }
                }
            });
        }
    }

    let loop_mac = mac.clone();
    let loop_address = address.clone();
    thread::spawn(move || {
        for request in server.incoming_requests() {
            if let Err(e) = handle_request(
                request,
                Arc::clone(&state),
                Arc::clone(&mdns),
                Arc::clone(&current_service_info),
                loop_mac.clone(),
                loop_address.clone(),
                config.clone(),
                config_path.clone(),
                is_temp,
            ) {
                error!("Error handling request: {}", e);
            }
        }
    });

    Ok(())
}

fn send_json_response(req: Request, status: StatusCode, body: serde_json::Value) -> Result<()> {
    let json_str = serde_json::to_string(&body)?;
    let response = Response::from_string(json_str)
        .with_status_code(status.as_u16() as i32)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
    req.respond(response)?;
    Ok(())
}

fn get_bearer_token(req: &Request) -> Option<String> {
    for header in req.headers() {
        let field_str = std::str::from_utf8(header.field.as_str().as_bytes()).unwrap_or("");
        if field_str.eq_ignore_ascii_case("authorization") {
            let val = std::str::from_utf8(header.value.as_str().as_bytes()).unwrap_or("");
            if val.to_lowercase().starts_with("bearer ") {
                return Some(val[7..].trim().to_string());
            }
        }
    }
    None
}

fn get_network_interfaces() -> serde_json::Value {
    let interfaces = match NetworkInterface::show() {
        Ok(itfs) => itfs,
        Err(e) => {
            error!("Failed to list network interfaces: {}", e);
            return json!([]);
        }
    };

    let list: Vec<serde_json::Value> = interfaces
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
        .map(|itf| {
            let ip_addresses: Vec<String> = itf.addr.iter()
                .filter(|a| a.ip().is_ipv4())
                .map(|a| a.ip().to_string())
                .collect();
            let ip_address = ip_addresses.first().cloned().unwrap_or_default();
            json!({
                "name": itf.name,
                "mac_address": itf.mac_addr,
                "ip_address": ip_address,
                "ip_addresses": ip_addresses,
            })
        })
        .collect();
    json!(list)
}

fn handle_interfaces(request: Request, state: &Arc<Mutex<DaemonState>>) -> Result<()> {
    let s = state.lock().unwrap();
    let provided_token = get_bearer_token(&request);
    
    let authenticated = match (&s.token, &s.setup_pin, &provided_token) {
        (Some(saved_token), _, Some(token)) if saved_token == token => true,
        (_, Some(setup_pin), Some(token)) if setup_pin == token => true,
        _ => false,
    };

    if !authenticated {
        warn!("Unauthorized /interfaces attempt from {:?}", request.remote_addr());
        send_json_response(request, StatusCode::UNAUTHORIZED, json!({
            "error": "invalid_token"
        }))?;
        return Ok(());
    }

    send_json_response(request, StatusCode::OK, get_network_interfaces())?;
    Ok(())
}

fn handle_status(request: Request, state: &Arc<Mutex<DaemonState>>) -> Result<()> {
    let paired = state.lock().unwrap().paired;
    let response_body = json!({
        "paired": paired,
        "os": get_os_name(),
        "version": env!("CARGO_PKG_VERSION")
    });
    debug!("Status response: {}", response_body);
    send_json_response(request, StatusCode::OK, response_body)?;
    Ok(())
}

fn handle_pair_verify(
    mut request: Request,
    state: Arc<Mutex<DaemonState>>,
    mac: String,
    config: crate::config::Config,
) -> Result<()> {
    let mut content = String::new();
    let _ = request.as_reader().read_to_string(&mut content);
    debug!("Received /pair/verify payload: {}", content);

    let mut s = state.lock().unwrap();
    let provided_token = get_bearer_token(&request);
    let pin_matched = match (&s.setup_pin, &provided_token) {
        (Some(setup_pin), Some(token)) => setup_pin == token,
        _ => false,
    };
    if !pin_matched {
        if s.paired && s.setup_pin.is_none() {
            warn!("Re-pair verify attempt from {:?} rejected: already paired and no setup PIN is set (run `grubstation reset-pin`)", request.remote_addr());
            send_json_response(request, StatusCode::CONFLICT, json!({
                "error": "already_paired",
                "hint": "Run `grubstation reset-pin` on the host to generate a new pairing PIN."
            }))?;
        } else {
            warn!("Unauthorized /pair/verify attempt from {:?}: invalid PIN", request.remote_addr());
            send_json_response(request, StatusCode::UNAUTHORIZED, json!({
                "error": "invalid_pin"
            }))?;
        }
        return Ok(());
    }

    // Extract/parse the host's current GRUB entries
    let entries = match (|| -> Result<Vec<String>> {
        if let Some(ref gc) = config.grub {
            Ok(crate::grub::parse_grub_entries(&gc.path)?)
        } else {
            if let Some(path) = crate::config::DEFAULT_GRUB_PATHS.iter().map(PathBuf::from).find(|p| p.exists()) {
                Ok(crate::grub::parse_grub_entries(&path)?)
            } else {
                Ok(Vec::new())
            }
        }
    })() {
        Ok(e) => e,
        Err(err) => {
            error!("Failed to parse GRUB entries: {}", err);
            Vec::new()
        }
    };

    let token = generate_token();
    s.token = Some(token.clone());

    let system_hostname = hostname::get()?.to_string_lossy().into_owned();
    let os_name = get_os_name();

    send_json_response(request, StatusCode::OK, json!({
        "success": true,
        "token": token,
        "mac": mac,
        "hostname": system_hostname,
        "os": os_name,
        "boot_options": entries
    }))?;

    info!("PIN verified successfully. Session token generated.");
    Ok(())
}

fn handle_pair(
    mut request: Request,
    state: Arc<Mutex<DaemonState>>,
    mdns: Arc<ServiceDaemon>,
    current_service_info: Arc<Mutex<ServiceInfo>>,
    _mac: String,
    _address: String,
    config: crate::config::Config,
    config_path: PathBuf,
    _is_temp: bool,
) -> Result<()> {
    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
    let mut s = state.lock().unwrap();
    let provided_token = get_bearer_token(&request);
    let token_matched = match (&s.token, &provided_token) {
        (Some(saved_token), Some(token)) => saved_token == token,
        _ => false,
    };
    if !token_matched {
        warn!("Unauthorized /pair attempt from {:?}: invalid session token", request.remote_addr());
        send_json_response(request, StatusCode::UNAUTHORIZED, json!({
            "error": "invalid_token"
        }))?;
        return Ok(());
    }

    let mut content = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut content) {
        send_json_response(request, StatusCode::BAD_REQUEST, json!({
            "error": format!("Failed to read request body: {}", e)
        }))?;
        return Ok(());
    }

    debug!("Received /pair payload: {}", content);

    let pair_req: PairRequest = match serde_json::from_str(&content) {
        Ok(req) => req,
        Err(e) => {
            error!("Invalid JSON payload: {}", e);
            send_json_response(request, StatusCode::BAD_REQUEST, json!({
                "error": format!("Invalid JSON payload: {}", e)
            }))?;
            return Ok(());
        }
    };

    // Resolve selected interface details
    let (mac, address) = match crate::config::resolve_interface_details(&pair_req.interface) {
        Ok(details) => details,
        Err(err) => {
            error!("Failed to resolve interface details for '{}': {}", pair_req.interface, err);
            send_json_response(request, StatusCode::BAD_REQUEST, json!({
                "error": format!("Failed to resolve interface details: {}", err)
            }))?;
            return Ok(());
        }
    };

    // Update config.yaml with the chosen interface
    let mut config = config.clone();
    if config.host.interface != pair_req.interface {
        info!("Updating interface configuration in config.yaml to '{}'", pair_req.interface);
        config.host.interface = pair_req.interface.clone();
        if let Ok(yaml) = serde_yaml::to_string(&config) {
            if let Err(e) = std::fs::write(&config_path, yaml) {
                error!("Failed to write updated config.yaml: {}", e);
            }
        }
    }

    // Extract/parse the host's current GRUB entries
    let entries = match (|| -> Result<Vec<String>> {
        if let Some(ref gc) = config.grub {
            Ok(crate::grub::parse_grub_entries(&gc.path)?)
        } else {
            if let Some(path) = crate::config::DEFAULT_GRUB_PATHS.iter().map(PathBuf::from).find(|p| p.exists()) {
                Ok(crate::grub::parse_grub_entries(&path)?)
            } else {
                Ok(Vec::new())
            }
        }
    })() {
        Ok(e) => e,
        Err(err) => {
            error!("Failed to parse GRUB entries: {}", err);
            Vec::new()
        }
    };

    // Always write/install the GRUB boot hook if GRUB is configured or defaults exist
    if config.grub.is_some() || crate::config::DEFAULT_GRUB_PATHS.iter().map(PathBuf::from).any(|p| p.exists()) {
        let mut grub_config = if let Some(ref gc) = config.grub {
            gc.clone()
        } else {
            let path = crate::config::DEFAULT_GRUB_PATHS
                .iter()
                .map(PathBuf::from)
                .find(|p| p.exists())
                .unwrap_or_else(|| PathBuf::from("/boot/grub/grub.cfg"));
            crate::config::GrubConfig {
                path,
                network_wait: 10,
                webhook_id: pair_req.webhook_id.clone(),
            }
        };
        grub_config.webhook_id = pair_req.webhook_id.clone();

        let derived_grub_boot_url = format!("{}/api/webhook/{}", pair_req.ha_url.trim_end_matches('/'), pair_req.webhook_id);
        if let Err(err) = crate::wizard::install_grub_hook(&config, &grub_config, Some(&derived_grub_boot_url), pair_req.update_grub) {
            error!("Failed to apply GRUB configuration: {}", err);
            send_json_response(request, StatusCode::INTERNAL_SERVER_ERROR, json!({
                "error": format!("Failed to apply GRUB configuration: {}", err)
            }))?;
            return Ok(());
        }
    }

    // Generate/finalize token
    s.paired = true;
    s.setup_pin = None;
    let token = s.token.clone().unwrap_or_else(generate_token);

    // Update mDNS advertisement
    let mut info = current_service_info.lock().unwrap();
    let fullname = info.get_fullname().to_string();
    let _ = mdns.unregister(&fullname);

    // Create new ServiceInfo with paired=true
    let service_type = "_grubstation._tcp.local.";
    let instance_name = address.clone();
    let system_hostname = hostname::get().unwrap().to_string_lossy().into_owned();
    let host_name = format!("{}.local.", system_hostname);
    let port = info.get_port();

    let mut properties = std::collections::HashMap::new();
    properties.insert("paired".to_string(), "true".to_string());

    if let Ok(new_info) = mdns_sd::ServiceInfo::new(
        service_type,
        &instance_name,
        &host_name,
        "",
        port,
        Some(properties),
    ) {
        let new_info = new_info.enable_addr_auto();
        if let Ok(()) = mdns.register(new_info.clone()) {
            *info = new_info;
        }
    }

    // Save pairing state and request data to state.json
    let state_file_data = PersistedState {
        paired: true,
        token: Some(token.clone()),
        setup_pin: None,
        webhook_id: Some(pair_req.webhook_id.clone()),
        api_key: Some(pair_req.api_key.clone()),
        ha_url: Some(pair_req.ha_url.clone()),
    };

    if let Ok(json_str) = serde_json::to_string_pretty(&state_file_data) {
        info!("Saving pairing state to {:?}", state_path);
        let _ = std::fs::write(&state_path, json_str);
    }

    send_json_response(request, StatusCode::OK, json!({
        "success": true
    }))?;

    info!("Pairing configuration hand-off completed successfully.");

    // Spawn background thread to perform initial sync of boot options after responding
    let ha_url = pair_req.ha_url.clone();
    let webhook_id = pair_req.webhook_id.clone();
    let api_key = pair_req.api_key.clone();
    let mac = mac.clone();
    std::thread::spawn(move || {
        // Sleep 500ms to let Home Assistant register the webhook endpoint
        std::thread::sleep(std::time::Duration::from_millis(500));
        info!("Performing initial sync of boot options to Home Assistant...");
        match crate::client::push_boot_options(
            &ha_url,
            &webhook_id,
            &api_key,
            &mac,
            &entries,
        ) {
            Ok(()) => {
                info!("Initial boot options sync successful!");
            }
            Err(err) => {
                error!("Initial boot options sync failed: {}", err);
            }
        }
    });

    Ok(())
}

fn handle_unpair(
    mut request: Request,
    state: Arc<Mutex<DaemonState>>,
    mdns: Arc<ServiceDaemon>,
    current_service_info: Arc<Mutex<ServiceInfo>>,
    _mac: String,
    address: String,
    config_path: PathBuf,
) -> Result<()> {
    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
    let provided_token = get_bearer_token(&request);
    let mut content = String::new();
    let _ = request.as_reader().read_to_string(&mut content);
    info!("Received /unpair request. Token: {:?}, Payload: {}", provided_token, content);
    let mut s = state.lock().unwrap();
    
    if !s.paired {
        send_json_response(request, StatusCode::BAD_REQUEST, json!({
            "error": "Not paired"
        }))?;
    } else if provided_token.is_none() || s.token.as_ref() != provided_token.as_ref() {
        warn!("Unauthorized /unpair attempt from {:?}", request.remote_addr());
        send_json_response(request, StatusCode::UNAUTHORIZED, json!({
            "error": "Unauthorized"
        }))?;
    } else {
        s.paired = false;
        s.token = None;

        // Update mDNS advertisement
        let mut info = current_service_info.lock().unwrap();
        let fullname = info.get_fullname().to_string();
        let _ = mdns.unregister(&fullname);

        // Create new ServiceInfo with paired=false
        let service_type = "_grubstation._tcp.local.";
        let instance_name = address.clone();
        let system_hostname = hostname::get().unwrap().to_string_lossy().into_owned();
        let host_name = format!("{}.local.", system_hostname);
        let port = info.get_port();

        let mut properties = std::collections::HashMap::new();
        properties.insert("paired".to_string(), "false".to_string());

        if let Ok(new_info) = mdns_sd::ServiceInfo::new(
            service_type,
            &instance_name,
            &host_name,
            "",
            port,
            Some(properties),
        ) {
            let new_info = new_info.enable_addr_auto();
            if let Ok(()) = mdns.register(new_info.clone()) {
                *info = new_info;
            }
        }

        // Delete state.json
        let _ = std::fs::remove_file(&state_path);

        send_json_response(request, StatusCode::OK, json!({
            "success": true
        }))?;
    }
    Ok(())
}

fn handle_update(
    mut request: Request,
    state: Arc<Mutex<DaemonState>>,
    mac: String,
    config: crate::config::Config,
    config_path: PathBuf,
) -> Result<()> {
    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");

    // Authenticate with the pairing token
    let provided_token = get_bearer_token(&request);
    let is_authorized = {
        let s = state.lock().unwrap();
        s.paired && provided_token.is_some() && s.token.as_ref() == provided_token.as_ref()
    };
    if !is_authorized {
        warn!("Unauthorized /update attempt from {:?}", request.remote_addr());
        send_json_response(request, StatusCode::UNAUTHORIZED, json!({ "error": "Unauthorized" }))?;
        return Ok(());
    }

    // Parse request body
    let mut content = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut content) {
        send_json_response(request, StatusCode::BAD_REQUEST, json!({
            "error": format!("Failed to read request body: {}", e)
        }))?;
        return Ok(());
    }
    debug!("Received /update payload: {}", content);
    let update_req: UpdateRequest = match serde_json::from_str(&content) {
        Ok(r) => r,
        Err(e) => {
            send_json_response(request, StatusCode::BAD_REQUEST, json!({
                "error": format!("Invalid JSON payload: {}", e)
            }))?;
            return Ok(());
        }
    };

    // Load current state.json and apply the updates
    let current = match std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str::<PersistedState>(&s).ok())
    {
        Some(s) => s,
        None => {
            send_json_response(request, StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "Failed to read state" }))?;
            return Ok(());
        }
    };

    let new_ha_url = update_req.ha_url
        .unwrap_or_else(|| current.ha_url.clone().unwrap_or_default());
    let new_grub_boot_url = if let Some(ref webhook_id) = current.webhook_id {
        format!("{}/api/webhook/{}", new_ha_url.trim_end_matches('/'), webhook_id)
    } else {
        String::new()
    };

    info!(
        "/update: ha_url={:?} derived_grub_boot_url={:?} update_grub={}",
        new_ha_url, new_grub_boot_url, update_req.update_grub
    );

    // Optionally re-run the GRUB hook with the new URL
    if update_req.update_grub {
        if let Some(ref grub_config) = config.grub {
            if let Err(e) = crate::wizard::install_grub_hook(
                &config,
                grub_config,
                Some(&new_grub_boot_url),
                true,
            ) {
                error!("Failed to apply GRUB configuration during /update: {}", e);
                send_json_response(request, StatusCode::INTERNAL_SERVER_ERROR, json!({
                    "error": format!("Failed to apply GRUB configuration: {}", e)
                }))?;
                return Ok(());
            }
        }
    }

    // Persist the updated state
    let updated = PersistedState {
        ha_url: Some(new_ha_url.clone()),
        ..current
    };
    if let Ok(json_str) = serde_json::to_string_pretty(&updated) {
        let _ = std::fs::write(&state_path, json_str);
    }

    send_json_response(request, StatusCode::OK, json!({ "success": true }))?;
    info!("/update: state persisted successfully.");

    // Trigger a re-sync to the new URL in the background
    if let (Some(webhook_id), Some(api_key)) = (updated.webhook_id, updated.api_key) {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            info!("/update: triggering re-sync to new HA URL...");
            let entries = if let Some(ref gc) = config.grub {
                crate::grub::parse_grub_entries(&gc.path).unwrap_or_default()
            } else {
                crate::config::DEFAULT_GRUB_PATHS
                    .iter()
                    .map(std::path::PathBuf::from)
                    .find(|p| p.exists())
                    .and_then(|p| crate::grub::parse_grub_entries(&p).ok())
                    .unwrap_or_default()
            };
            match crate::client::push_boot_options(
                &new_ha_url,
                &webhook_id,
                &api_key,
                &mac,
                &entries,
            ) {
                Ok(()) => info!("/update: re-sync successful."),
                Err(e) => error!("/update: re-sync failed: {}", e),
            }
        });
    }

    Ok(())
}

fn handle_shutdown(
    mut request: Request,
    state: Arc<Mutex<DaemonState>>,
) -> Result<()> {
    let provided_token = get_bearer_token(&request);
    let mut content = String::new();
    let _ = request.as_reader().read_to_string(&mut content);
    info!("Received /shutdown request. Token: {:?}, Payload: {}", provided_token, content);
    let is_authorized = {
        let s = state.lock().unwrap();
        s.paired && provided_token.is_some() && s.token.as_ref() == provided_token.as_ref()
    };

    if !is_authorized {
        warn!("Unauthorized /shutdown attempt from {:?}", request.remote_addr());
        send_json_response(request, StatusCode::UNAUTHORIZED, json!({
            "error": "Unauthorized"
        }))?;
    } else {
        send_json_response(request, StatusCode::OK, json!({
            "success": true,
            "message": "Shutting down..."
        }))?;

        // Trigger shutdown asynchronously after a short sleep to allow TCP flush
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if let Err(e) = trigger_shutdown() {
                error!("Failed to shut down system: {}", e);
            }
        });
    }
    Ok(())
}

fn handle_request(
    request: Request,
    state: Arc<Mutex<DaemonState>>,
    mdns: Arc<ServiceDaemon>,
    current_service_info: Arc<Mutex<ServiceInfo>>,
    mac: String,
    address: String,
    config: crate::config::Config,
    config_path: PathBuf,
    is_temp: bool,
) -> Result<()> {
    let method = request.method().as_str();
    let url = request.url();

    match (method, url) {
        ("GET", "/status") => handle_status(request, &state)?,
        ("GET", "/interfaces") => handle_interfaces(request, &state)?,
        ("POST", "/pair/verify") => handle_pair_verify(
            request,
            state,
            mac,
            config,
        )?,
        ("POST", "/pair") => handle_pair(
            request,
            state,
            mdns,
            current_service_info,
            mac,
            address,
            config,
            config_path,
            is_temp,
        )?,
        ("POST", "/update") => handle_update(
            request,
            state,
            mac,
            config,
            config_path,
        )?,
        ("POST", "/unpair") => handle_unpair(
            request,
            state,
            mdns,
            current_service_info,
            mac,
            address,
            config_path,
        )?,
        ("POST", "/shutdown") => handle_shutdown(request, state)?,
        _ => {
            send_json_response(request, StatusCode::NOT_FOUND, json!({
                "error": "Not Found"
            }))?;
        }
    }

    Ok(())
}

fn generate_token() -> String {
    std::iter::repeat_with(fastrand::alphanumeric)
        .take(32)
        .collect()
}

fn trigger_shutdown() -> Result<()> {
    if cfg!(target_os = "windows") {
        if let Err(e) = std::process::Command::new("shutdown")
            .args(&["/s", "/t", "0"])
            .status()
        {
            warn!("Failed to run shutdown /s /t 0: {}. Trying shutdown /s /f /t 0...", e);
            std::process::Command::new("shutdown")
                .args(&["/s", "/f", "/t", "0"])
                .status()?;
        }
    } else {
        let mut errs = Vec::new();
        
        // 1. Try shutdown -h now
        match std::process::Command::new("shutdown")
            .args(&["-h", "now"])
            .status()
        {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => errs.push(format!("shutdown -h now exited with status: {}", status)),
            Err(e) => errs.push(format!("failed to run shutdown: {}", e)),
        }

        // 2. Try poweroff
        match std::process::Command::new("poweroff").status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => errs.push(format!("poweroff exited with status: {}", status)),
            Err(e) => errs.push(format!("failed to run poweroff: {}", e)),
        }

        // 3. Try systemctl poweroff
        match std::process::Command::new("systemctl").arg("poweroff").status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => errs.push(format!("systemctl poweroff exited with status: {}", status)),
            Err(e) => errs.push(format!("failed to run systemctl: {}", e)),
        }

        // 4. Try init 0
        match std::process::Command::new("init").arg("0").status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => errs.push(format!("init 0 exited with status: {}", status)),
            Err(e) => errs.push(format!("failed to run init: {}", e)),
        }

        anyhow::bail!("All shutdown commands failed: {:?}", errs);
    }
    Ok(())
}

pub fn get_os_name() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
            for line in content.lines() {
                if let Some(stripped) = line.strip_prefix("PRETTY_NAME=") {
                    return stripped.trim_matches('"').to_string();
                }
            }
        }
    }
    std::env::consts::OS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_pairing_endpoint() -> Result<()> {
        let dir = tempdir()?;
        let temp_hook_path = dir.path().join("99_grubstation");
        unsafe {
            std::env::set_var("GRUBSTATION_HOOK_PATH", temp_hook_path.to_str().unwrap());
        }
        let grub_path = dir.path().join("grub.cfg");
        let mut grub_file = File::create(&grub_path)?;
        writeln!(grub_file, "menuentry 'Ubuntu' {{")?;
        writeln!(grub_file, "  echo 1")?;
        writeln!(grub_file, "}}")?;
        writeln!(grub_file, "submenu 'Advanced' {{")?;
        writeln!(grub_file, "  menuentry 'Kernel 2' {{")?;
        writeln!(grub_file, "    echo 2")?;
        writeln!(grub_file, "  }}")?;
        writeln!(grub_file, "}}")?;
        grub_file.sync_all()?;

        let config_path = dir.path().join("config.yaml");

        // Write mock state.json containing setup_pin and paired: false
        let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
        let state_data = PersistedState {
            paired: false,
            token: None,
            setup_pin: Some("test-setup-pin".to_string()),
            webhook_id: None,
            api_key: None,
            ha_url: None,
        };
        std::fs::write(&state_path, serde_json::to_string_pretty(&state_data)?)?;

        // We bind to ephemeral port (0) for daemon and mock HA
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let daemon_port = listener.local_addr()?.port();
        drop(listener);

        let config = crate::config::Config {
            host: crate::config::HostConfig {
                interface: "lo".to_string(),
            },
            daemon: Some(crate::config::DaemonConfig { port: daemon_port }),
            grub: Some(crate::config::GrubConfig {
                path: grub_path,
                network_wait: 10,
                webhook_id: "init-webhook".to_string(),
            }),
            webhook_id: None,
            api_key: None,
            ha_url: None,
        };

        let (mac, address) = crate::config::resolve_interface_details("lo").unwrap();
        let expected_mac = mac.clone();

        // Start mock HA webhook server
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
                assert_eq!(json_body["mac"], expected_mac);
                let options = json_body["boot_options"].as_array().unwrap();
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].as_str().unwrap(), "Ubuntu");
                assert_eq!(options[1].as_str().unwrap(), "Advanced>Kernel 2");

                let response = tiny_http::Response::from_string("{\"status\": \"ok\"}")
                    .with_status_code(StatusCode::OK.as_u16() as i32);
                req.respond(response).unwrap();
            }
        });

        // Start daemon server
        let mdns = ServiceDaemon::new()?;
        let service_info = mdns_sd::ServiceInfo::new(
            "_grubstation._tcp.local.",
            "127.0.0.1",
            "test.local.",
            "",
            daemon_port,
            None,
        )?.enable_addr_auto();
        
        start_server(&config, config_path.clone(), mdns, service_info, mac, address, false)?;
        
        // Wait a tiny bit for server to spin up
        std::thread::sleep(std::time::Duration::from_millis(100));

        // 1. Verify PIN and retrieve details
        let res_verify = ureq::post(&format!("http://127.0.0.1:{}/pair/verify", daemon_port))
            .set("Authorization", "Bearer test-setup-pin")
            .send_json(serde_json::json!({}))?;
        assert_eq!(res_verify.status(), StatusCode::OK.as_u16());
        let verify_json: serde_json::Value = res_verify.into_json()?;
        assert!(verify_json["success"].as_bool().unwrap());
        let token = verify_json["token"].as_str().unwrap();
        assert!(!token.is_empty());
        assert!(verify_json["mac"].as_str().is_some());
        assert_eq!(verify_json["hostname"].as_str().unwrap(), hostname::get()?.to_string_lossy().into_owned());
        let options = verify_json["boot_options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].as_str().unwrap(), "Ubuntu");
        assert_eq!(options[1].as_str().unwrap(), "Advanced>Kernel 2");

        // 1b. Test /interfaces endpoint
        let res_interfaces = ureq::get(&format!("http://127.0.0.1:{}/interfaces", daemon_port))
            .set("Authorization", &format!("Bearer {}", token))
            .call()?;
        assert_eq!(res_interfaces.status(), StatusCode::OK.as_u16());
        let interfaces_json: serde_json::Value = res_interfaces.into_json()?;
        let interfaces_list = interfaces_json.as_array().unwrap();
        assert!(!interfaces_list.is_empty());

        // Test unauthorized access to /interfaces
        let res_unauth = ureq::get(&format!("http://127.0.0.1:{}/interfaces", daemon_port))
            .set("Authorization", "Bearer invalid-token")
            .call();
        assert!(res_unauth.is_err());

        // 2. Perform pair request (config hand-off) to the daemon
        let pair_payload = serde_json::json!({
            "webhook_id": "test-webhook-id",
            "api_key": "test-api-key",
            "ha_url": format!("http://127.0.0.1:{}", ha_port),
            "update_grub": false,
            "interface": "lo",
        });

        let res = ureq::post(&format!("http://127.0.0.1:{}/pair", daemon_port))
            .set("Authorization", &format!("Bearer {}", token))
            .send_json(pair_payload)?;

        assert_eq!(res.status(), StatusCode::OK.as_u16());
        let res_json: serde_json::Value = res.into_json()?;
        assert!(res_json["success"].as_bool().unwrap());

        // Check if state.json is saved correctly
        let state_path = dir.path().join("state.json");
        assert!(state_path.exists());
        let state_content = std::fs::read_to_string(&state_path)?;
        let state_json: serde_json::Value = serde_json::from_str(&state_content)?;
        assert!(state_json["paired"].as_bool().unwrap());
        assert_eq!(state_json["token"].as_str().unwrap(), token);
        assert_eq!(state_json["webhook_id"].as_str().unwrap(), "test-webhook-id");
        assert_eq!(state_json["api_key"].as_str().unwrap(), "test-api-key");
        assert_eq!(state_json["ha_url"].as_str().unwrap(), format!("http://127.0.0.1:{}", ha_port));

        // Wait for the mock HA thread to finish
        handle.join().unwrap();

        Ok(())
    }

    #[test]
    fn test_pairing_endpoint_with_setup_pin() -> Result<()> {
        let dir = tempdir()?;
        let temp_hook_path = dir.path().join("99_grubstation");
        unsafe {
            std::env::set_var("GRUBSTATION_HOOK_PATH", temp_hook_path.to_str().unwrap());
        }
        let grub_path = dir.path().join("grub.cfg");
        let mut grub_file = File::create(&grub_path)?;
        writeln!(grub_file, "menuentry 'Ubuntu' {{")?;
        writeln!(grub_file, "  echo 1")?;
        writeln!(grub_file, "}}")?;
        grub_file.sync_all()?;

        let config_path = dir.path().join("config.yaml");

        // We bind to ephemeral port (0) for daemon and mock HA
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let daemon_port = listener.local_addr()?.port();
        drop(listener);

        let config = crate::config::Config {
            host: crate::config::HostConfig {
                interface: "lo".to_string(),
            },
            daemon: Some(crate::config::DaemonConfig { port: daemon_port }),
            grub: Some(crate::config::GrubConfig {
                path: grub_path,
                network_wait: 10,
                webhook_id: "init-webhook".to_string(),
            }),
            webhook_id: None,
            api_key: None,
            ha_url: None,
        };

        // Write mock state.json containing setup_pin and paired: false
        let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
        let state_data = PersistedState {
            paired: false,
            token: None,
            setup_pin: Some("654321".to_string()),
            webhook_id: None,
            api_key: None,
            ha_url: None,
        };
        std::fs::write(&state_path, serde_json::to_string_pretty(&state_data)?)?;

        // Start mock HA webhook server
        let ha_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let ha_port = ha_listener.local_addr()?.port();
        let ha_server = tiny_http::Server::from_listener(ha_listener, None)
            .map_err(|e| anyhow::anyhow!("Failed to start HA server: {}", e))?;

        let handle = std::thread::spawn(move || {
            if let Some(req) = ha_server.incoming_requests().next() {
                assert_eq!(req.url(), "/api/webhook/test-webhook-id");
                let response = tiny_http::Response::from_string("{\"status\": \"ok\"}")
                    .with_status_code(StatusCode::OK.as_u16() as i32);
                req.respond(response).unwrap();
            }
        });

        // Start daemon server
        let mdns = ServiceDaemon::new()?;
        let service_info = mdns_sd::ServiceInfo::new(
            "_grubstation._tcp.local.",
            "127.0.0.1",
            "test.local.",
            "",
            daemon_port,
            None,
        )?.enable_addr_auto();
        
        let (mac, address) = crate::config::resolve_interface_details("lo").unwrap();
        start_server(&config, config_path.clone(), mdns, service_info, mac, address, false)?;
        
        // Wait a tiny bit for server to spin up
        std::thread::sleep(std::time::Duration::from_millis(100));

        let pair_payload = serde_json::json!({
            "webhook_id": "test-webhook-id",
            "api_key": "test-api-key",
            "ha_url": format!("http://127.0.0.1:{}", ha_port),
            "update_grub": false,
            "interface": "lo",
        });

        // 1. Attempt to verify PIN with no auth token (should fail with 401)
        let res_no_auth = ureq::post(&format!("http://127.0.0.1:{}/pair/verify", daemon_port))
            .send_json(serde_json::json!({}));
        assert!(res_no_auth.is_err());
        if let Err(ureq::Error::Status(code, _)) = res_no_auth {
            assert_eq!(code, StatusCode::UNAUTHORIZED.as_u16());
        } else {
            panic!("Expected status error 401");
        }

        // 2. Attempt to verify PIN with invalid auth token (should fail with 401)
        let res_bad_auth = ureq::post(&format!("http://127.0.0.1:{}/pair/verify", daemon_port))
            .set("Authorization", "Bearer 111111")
            .send_json(serde_json::json!({}));
        assert!(res_bad_auth.is_err());
        if let Err(ureq::Error::Status(code, _)) = res_bad_auth {
            assert_eq!(code, StatusCode::UNAUTHORIZED.as_u16());
        } else {
            panic!("Expected status error 401");
        }

        // 3. Attempt to verify PIN with correct setup pin (should succeed)
        let res_verify = ureq::post(&format!("http://127.0.0.1:{}/pair/verify", daemon_port))
            .set("Authorization", "Bearer 654321")
            .send_json(serde_json::json!({}))?;

        assert_eq!(res_verify.status(), StatusCode::OK.as_u16());
        let verify_json: serde_json::Value = res_verify.into_json()?;
        assert!(verify_json["success"].as_bool().unwrap());
        let token = verify_json["token"].as_str().unwrap();

        // 4. Attempt to configure pairing with invalid token (should fail with 401)
        let res_bad_config = ureq::post(&format!("http://127.0.0.1:{}/pair", daemon_port))
            .set("Authorization", "Bearer bad-token")
            .send_json(pair_payload.clone());
        assert!(res_bad_config.is_err());

        // 5. Attempt to configure pairing with correct token (should succeed)
        let res = ureq::post(&format!("http://127.0.0.1:{}/pair", daemon_port))
            .set("Authorization", &format!("Bearer {}", token))
            .send_json(pair_payload)?;

        assert_eq!(res.status(), StatusCode::OK.as_u16());
        let res_json: serde_json::Value = res.into_json()?;
        assert!(res_json["success"].as_bool().unwrap());

        // Wait for the mock HA thread to finish
        handle.join().unwrap();

        Ok(())
    }
}
