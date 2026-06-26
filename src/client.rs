use serde_json::json;
use anyhow::Result;

pub fn build_boot_options_payload(mac: &str, entries: &[String]) -> serde_json::Value {
    json!({
        "action": "update_boot_options",
        "mac": mac,
        "boot_options": entries,
    })
}

pub fn push_boot_options(
    ha_daemon_url: &str,
    webhook_id: &str,
    api_key: &str,
    mac: &str,
    entries: &[String],
) -> Result<()> {
    let webhook_url = format!(
        "{}/api/webhook/{}",
        ha_daemon_url.trim_end_matches('/'),
        webhook_id
    );

    let payload = build_boot_options_payload(mac, entries);
    log::info!("Sending boot options payload to HA (webhook: {}): {}", webhook_url, serde_json::to_string(&payload).unwrap_or_default());

    let mut request = ureq::post(&webhook_url)
        .timeout(std::time::Duration::from_secs(10));
    if !api_key.trim().is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", api_key));
    }

    let response_result = request.send_json(payload);
    
    let response = match response_result {
        Ok(resp) => {
            log::info!("Received successful response from HA (status {}).", resp.status());
            resp
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            log::error!("Received HTTP error response from HA (status {}): {}", code, body);
            return Err(anyhow::anyhow!("HTTP error response (status {}): {}", code, body));
        }
        Err(ureq::Error::Transport(t)) => {
            log::error!("Transport error communicating with HA: {}", t);
            return Err(anyhow::anyhow!("Transport error: {}", t));
        }
    };

    if response.status() >= 200 && response.status() < 300 {
        let body = response.into_string().unwrap_or_default();
        log::info!("HA Response body: {}", body);
        let trimmed = body.trim();
        if trimmed.is_empty() {
            log::error!("HA webhook returned 200 but body is empty, indicating it is unregistered");
            return Err(anyhow::anyhow!("Webhook unregistered on Home Assistant"));
        }
        let body_lower = trimmed.to_lowercase();
        if !body_lower.contains("ok") {
            log::error!("HA webhook returned 200 but body indicates it is unregistered (no 'ok' status): {}", body);
            return Err(anyhow::anyhow!("Webhook unregistered on Home Assistant"));
        }
        Ok(())
    } else {
        anyhow::bail!(
            "Failed to push boot options to Home Assistant. HTTP status: {}",
            response.status()
        );
    }
}
