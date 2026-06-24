use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::thread;
use tiny_http::{Server, Response, Request, Header};
use serde_json::json;
use mdns_sd::{ServiceDaemon, ServiceInfo};

struct DaemonState {
    paired: bool,
    token: Option<String>,
}

pub fn start_server(
    config: &crate::config::Config,
    mdns: ServiceDaemon,
    service_info: ServiceInfo,
) -> Result<()> {
    let port = config.daemon.as_ref().map(|d| d.port).unwrap_or(crate::config::DEFAULT_DAEMON_PORT);
    let server = Server::http(format!("0.0.0.0:{}", port))
        .map_err(|e| anyhow::anyhow!("Failed to start HTTP server: {}", e))?;

    let state = Arc::new(Mutex::new(DaemonState {
        paired: false,
        token: None,
    }));

    let host_config = config.host.clone();
    let mdns = Arc::new(mdns);
    let current_service_info = Arc::new(Mutex::new(service_info));

    thread::spawn(move || {
        for request in server.incoming_requests() {
            let state = Arc::clone(&state);
            let mdns = Arc::clone(&mdns);
            let current_service_info = Arc::clone(&current_service_info);
            let host_config = host_config.clone();

            thread::spawn(move || {
                if let Err(e) = handle_request(request, state, mdns, current_service_info, host_config) {
                    eprintln!("Error handling request: {}", e);
                }
            });
        }
    });

    Ok(())
}

fn handle_request(
    request: Request,
    state: Arc<Mutex<DaemonState>>,
    mdns: Arc<ServiceDaemon>,
    current_service_info: Arc<Mutex<ServiceInfo>>,
    host_config: crate::config::HostConfig,
) -> Result<()> {
    let method = request.method().as_str();
    let url = request.url();

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

                // Trigger shutdown
                trigger_shutdown()?;
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
        std::process::Command::new("shutdown")
            .args(&["/s", "/t", "0"])
            .status()?;
    } else {
        std::process::Command::new("shutdown")
            .args(&["-h", "now"])
            .status()?;
    }
    Ok(())
}
