use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::thread;
use tiny_http::{Server, Response, Request, Header};
use serde_json::json;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PairRequest {
    pub webhook_id: String,
    pub api_key: String,
    pub ha_daemon_url: String,
    pub ha_grub_url: String,
    pub apply_config: bool,
}

struct DaemonState {
    paired: bool,
    token: Option<String>,
}

pub fn start_server(
    config: &crate::config::Config,
    config_path: PathBuf,
    mdns: ServiceDaemon,
    service_info: ServiceInfo,
) -> Result<()> {
    let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(crate::config::DEFAULT_DAEMON_PORT);
    let server = Server::http(format!("0.0.0.0:{}", port))
        .map_err(|e| anyhow::anyhow!("Failed to start HTTP server: {}", e))?;

    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");
    
    let mut initial_paired = false;
    let mut initial_token = None;

    if state_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&state_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                initial_paired = val["paired"].as_bool().unwrap_or(false);
                initial_token = val["token"].as_str().map(|s| s.to_string());
            }
        }
    }

    let state = Arc::new(Mutex::new(DaemonState {
        paired: initial_paired,
        token: initial_token,
    }));

    let host_config = config.host.clone();
    let mdns = Arc::new(mdns);
    let current_service_info = Arc::new(Mutex::new(service_info));
    let config = config.clone();

    // If loaded state is already paired, update the mDNS advertisement
    if initial_paired {
        let mut info = current_service_info.lock().unwrap();
        let fullname = info.get_fullname().to_string();
        let _ = mdns.unregister(&fullname);

        let mut properties = std::collections::HashMap::new();
        properties.insert("mac".to_string(), host_config.mac.clone());
        properties.insert("paired".to_string(), "true".to_string());

        if let Ok(new_info) = mdns_sd::ServiceInfo::new(
            "_grubstation._tcp.local.",
            &host_config.address,
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

    thread::spawn(move || {
        for request in server.incoming_requests() {
            let state = Arc::clone(&state);
            let mdns = Arc::clone(&mdns);
            let current_service_info = Arc::clone(&current_service_info);
            let host_config = host_config.clone();
            let config = config.clone();
            let config_path = config_path.clone();

            thread::spawn(move || {
                if let Err(e) = handle_request(request, state, mdns, current_service_info, host_config, config, config_path) {
                    eprintln!("Error handling request: {}", e);
                }
            });
        }
    });

    Ok(())
}

fn handle_request(
    mut request: Request,
    state: Arc<Mutex<DaemonState>>,
    mdns: Arc<ServiceDaemon>,
    current_service_info: Arc<Mutex<ServiceInfo>>,
    host_config: crate::config::HostConfig,
    config: crate::config::Config,
    config_path: PathBuf,
) -> Result<()> {
    let method = request.method().as_str();
    let url = request.url();

    let state_path = config_path.parent().unwrap_or(Path::new(".")).join("state.json");

    // Helper to send JSON responses
    let send_json = |req: Request, status: u32, body: serde_json::Value| -> Result<()> {
        let json_str = serde_json::to_string(&body)?;
        let response = Response::from_string(json_str)
            .with_status_code(status as i32)
            .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
        req.respond(response)?;
        Ok(())
    };

    // Extract Bearer token from Authorization header
    let get_bearer_token = |req: &Request| -> Option<String> {
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
    };

    match (method, url) {
        ("GET", "/status") => {
            let (paired, token_exists) = {
                let s = state.lock().unwrap();
                (s.paired, s.token.is_some())
            };
            send_json(request, 200, json!({
                "paired": paired,
                "token_configured": token_exists
            }))?;
        }
        ("POST", "/pair") => {
            let mut s = state.lock().unwrap();
            if s.paired {
                send_json(request, 409, json!({
                    "error": "Already paired"
                }))?;
            } else {
                let mut content = String::new();
                if let Err(e) = request.as_reader().read_to_string(&mut content) {
                    send_json(request, 400, json!({
                        "error": format!("Failed to read request body: {}", e)
                    }))?;
                    return Ok(());
                }

                let pair_req: PairRequest = match serde_json::from_str(&content) {
                    Ok(req) => req,
                    Err(e) => {
                        send_json(request, 400, json!({
                            "error": format!("Invalid JSON payload: {}", e)
                        }))?;
                        return Ok(());
                    }
                };

                // Extract/parse the host's current GRUB entries
                let entries = match (|| -> Result<Vec<String>> {
                    if let Some(ref gc) = config.grub {
                        Ok(crate::grub::parse_grub_entries(&gc.path)?)
                    } else {
                        if let Some(path) = crate::config::DEFAULT_GRUB_PATHS.iter().map(PathBuf::from).find(|p| p.exists()) {
                            Ok(crate::grub::parse_grub_entries(&path)?)
                        } else {
                            anyhow::bail!("No GRUB configuration file found at default paths.")
                        }
                    }
                })() {
                    Ok(e) => e,
                    Err(err) => {
                        send_json(request, 500, json!({
                            "error": format!("Failed to parse GRUB entries: {}", err)
                        }))?;
                        return Ok(());
                    }
                };

                // Push initial boot options to Home Assistant
                if let Err(err) = crate::grub::push_boot_options(
                    &pair_req.ha_daemon_url,
                    &pair_req.webhook_id,
                    &pair_req.api_key,
                    &host_config.mac,
                    &entries,
                ) {
                    send_json(request, 500, json!({
                        "error": format!("Failed to push initial boot options: {}", err)
                    }))?;
                    return Ok(());
                }

                // If apply_config is true, install/apply the GRUB boot hook
                if pair_req.apply_config {
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

                    if let Err(err) = crate::wizard::install_grub_hook(&config, &grub_config, Some(&pair_req.ha_grub_url)) {
                        send_json(request, 500, json!({
                            "error": format!("Failed to apply GRUB configuration: {}", err)
                        }))?;
                        return Ok(());
                    }
                }

                // Generate a random token
                let token = generate_token();
                s.paired = true;
                s.token = Some(token.clone());

                // Update mDNS advertisement
                let mut info = current_service_info.lock().unwrap();
                let fullname = info.get_fullname().to_string();
                let _ = mdns.unregister(&fullname);

                // Create new ServiceInfo with paired=true
                let service_type = "_grubstation._tcp.local.";
                let instance_name = host_config.address.clone();
                let system_hostname = hostname::get().unwrap().to_string_lossy().into_owned();
                let host_name = format!("{}.local.", system_hostname);
                let port = info.get_port();

                let mut properties = std::collections::HashMap::new();
                properties.insert("mac".to_string(), host_config.mac.clone());
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
                let state_file_data = serde_json::json!({
                    "paired": true,
                    "token": token,
                    "webhook_id": pair_req.webhook_id,
                    "api_key": pair_req.api_key,
                    "ha_daemon_url": pair_req.ha_daemon_url,
                    "ha_grub_url": pair_req.ha_grub_url,
                });

                if let Ok(json_str) = serde_json::to_string_pretty(&state_file_data) {
                    let _ = std::fs::write(&state_path, json_str);
                }

                send_json(request, 200, json!({
                    "success": true,
                    "token": token
                }))?;
            }
        }
        ("POST", "/unpair") => {
            let provided_token = get_bearer_token(&request);
            let mut s = state.lock().unwrap();
            
            if !s.paired {
                send_json(request, 400, json!({
                    "error": "Not paired"
                }))?;
            } else if provided_token.is_none() || s.token.as_ref() != provided_token.as_ref() {
                send_json(request, 401, json!({
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
                let instance_name = host_config.address.clone();
                let system_hostname = hostname::get().unwrap().to_string_lossy().into_owned();
                let host_name = format!("{}.local.", system_hostname);
                let port = info.get_port();

                let mut properties = std::collections::HashMap::new();
                properties.insert("mac".to_string(), host_config.mac.clone());
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

                send_json(request, 200, json!({
                    "success": true
                }))?;
            }
        }
        ("POST", "/shutdown") => {
            let provided_token = get_bearer_token(&request);
            let is_authorized = {
                let s = state.lock().unwrap();
                s.paired && provided_token.is_some() && s.token.as_ref() == provided_token.as_ref()
            };

            if !is_authorized {
                send_json(request, 401, json!({
                    "error": "Unauthorized"
                }))?;
            } else {
                send_json(request, 200, json!({
                    "success": true,
                    "message": "Shutting down..."
                }))?;

                // Trigger shutdown asynchronously after a short sleep to allow TCP flush
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if let Err(e) = trigger_shutdown() {
                        eprintln!("Failed to shut down system: {}", e);
                    }
                });
            }
        }
        _ => {
            send_json(request, 404, json!({
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
            eprintln!("Failed to run shutdown /s /t 0: {}. Trying shutdown /s /f /t 0...", e);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_pairing_endpoint() -> Result<()> {
        let dir = tempdir()?;
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

        // We bind to ephemeral port (0) for daemon and mock HA
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let daemon_port = listener.local_addr()?.port();
        drop(listener);

        let config = crate::config::Config {
            host: crate::config::HostConfig {
                mac: "00:11:22:33:44:55".to_string(),
                address: "127.0.0.1".to_string(),
            },
            daemon: Some(crate::config::DaemonConfig { port: daemon_port }),
            wake_on_lan: None,
            grub: Some(crate::config::GrubConfig {
                path: grub_path,
                network_wait: 10,
                webhook_id: "init-webhook".to_string(),
            }),
            webhook_id: None,
            api_key: None,
            ha_daemon_url: None,
            ha_grub_url: None,
        };

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
                assert_eq!(json_body["mac"], "00:11:22:33:44:55");
                let options = json_body["boot_options"].as_array().unwrap();
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].as_str().unwrap(), "Ubuntu");
                assert_eq!(options[1].as_str().unwrap(), "Advanced>Kernel 2");

                let response = tiny_http::Response::from_string("{\"success\": true}")
                    .with_status_code(200);
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
        
        start_server(&config, config_path.clone(), mdns, service_info)?;
        
        // Wait a tiny bit for server to spin up
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Perform pair request to the daemon
        let pair_payload = serde_json::json!({
            "webhook_id": "test-webhook-id",
            "api_key": "test-api-key",
            "ha_daemon_url": format!("http://127.0.0.1:{}", ha_port),
            "ha_grub_url": "http://127.0.0.1/grub",
            "apply_config": false,
        });

        let res = ureq::post(&format!("http://127.0.0.1:{}/pair", daemon_port))
            .send_json(pair_payload)?;

        assert_eq!(res.status(), 200);
        let res_json: serde_json::Value = res.into_json()?;
        assert!(res_json["success"].as_bool().unwrap());
        let token = res_json["token"].as_str().unwrap();
        assert!(!token.is_empty());

        // Check if state.json is saved correctly
        let state_path = dir.path().join("state.json");
        assert!(state_path.exists());
        let state_content = std::fs::read_to_string(&state_path)?;
        let state_json: serde_json::Value = serde_json::from_str(&state_content)?;
        assert!(state_json["paired"].as_bool().unwrap());
        assert_eq!(state_json["token"].as_str().unwrap(), token);
        assert_eq!(state_json["webhook_id"].as_str().unwrap(), "test-webhook-id");
        assert_eq!(state_json["api_key"].as_str().unwrap(), "test-api-key");
        assert_eq!(state_json["ha_daemon_url"].as_str().unwrap(), format!("http://127.0.0.1:{}", ha_port));
        assert_eq!(state_json["ha_grub_url"].as_str().unwrap(), "http://127.0.0.1/grub");

        // Wait for the mock HA thread to finish
        handle.join().unwrap();

        Ok(())
    }
}
