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

    let mut request = ureq::post(&webhook_url)
        .timeout(std::time::Duration::from_secs(10));
    if !api_key.trim().is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", api_key));
    }

    let response = request.send_json(payload).map_err(|e| match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            anyhow::anyhow!("HTTP error response (status {}): {}", code, body)
        }
        ureq::Error::Transport(t) => {
            anyhow::anyhow!("Transport error: {}", t)
        }
    })?;

    if response.status() >= 200 && response.status() < 300 {
        Ok(())
    } else {
        anyhow::bail!(
            "Failed to push boot options to Home Assistant. HTTP status: {}",
            response.status()
        );
    }
}
