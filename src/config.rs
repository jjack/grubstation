use config::{Config as ConfigLoader, File, FileFormat};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use validator::Validate;
use network_interface::NetworkInterfaceConfig;

pub const DEFAULT_BROADCAST_ADDRESS: &str = "255.255.255.255";
pub const DEFAULT_BROADCAST_PORT: u16 = 9;
pub const DEFAULT_DAEMON_PORT: u16 = 8081;

pub const DEFAULT_GRUB_PATHS: &[&str] = &["/boot/grub/grub.cfg", "/boot/grub2/grub.cfg"];

const MIN_BROADCAST_PORT: u16 = 1;
const MIN_PORT: u16 = 1025;
const MAX_PORT: u16 = 65535;

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
pub struct Config {
    #[validate(nested)]
    pub host: HostConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub daemon: Option<DaemonConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub wake_on_lan: Option<WakeOnLanConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub grub: Option<GrubConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ha_daemon_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ha_grub_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
pub struct HostConfig {
    #[validate(length(min = 1))]
    pub interface: String,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
pub struct DaemonConfig {
    #[validate(range(min = MIN_PORT, max = MAX_PORT))]
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
pub struct WakeOnLanConfig {
    #[validate(custom(function = "validate_ipv4"))]
    pub broadcast_address: String,
    #[validate(range(min = MIN_BROADCAST_PORT, max = MAX_PORT))]
    pub broadcast_port: u16,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
pub struct GrubConfig {
    #[validate(custom(function = "validate_file_exists"))]
    pub path: PathBuf,
    #[serde(default = "default_network_wait")]
    #[validate(range(min = 0, max = 300))]
    pub network_wait: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub webhook_id: String,
}

fn default_network_wait() -> u32 {
    10
}

fn validate_ipv4(address: &str) -> Result<(), validator::ValidationError> {
    if address.parse::<std::net::Ipv4Addr>().is_ok() {                                                                                                                                                
        Ok(())                                                                                                                                                                                        
    } else {                                                                                                                                                                                          
        Err(validator::ValidationError::new("invalid_ipv4"))                                                                                                                                          
    }  
}

fn validate_file_exists(path: &Path) -> Result<(), validator::ValidationError> {
    if path.exists() && path.is_file() {
        Ok(())
    } else {
        Err(validator::ValidationError::new("file_not_found"))
    }
}

pub fn resolve_interface_details(interface_name: &str) -> anyhow::Result<(String, String)> {
    let interfaces = network_interface::NetworkInterface::show()
        .map_err(|e| anyhow::anyhow!("Failed to list network interfaces: {}", e))?;
    let itf = interfaces.into_iter().find(|i| i.name == interface_name)
        .ok_or_else(|| anyhow::anyhow!("Interface '{}' not found", interface_name))?;
    let mac = itf.mac_addr.ok_or_else(|| anyhow::anyhow!("Interface '{}' has no MAC address", interface_name))?;
    let addr = itf.addr.into_iter()
        .find(|a| a.ip().is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("Interface '{}' has no IPv4 address", interface_name))?;
    Ok((mac, addr.ip().to_string()))
}

pub fn get_default_config_path() -> PathBuf {
    if cfg!(target_os = "linux") {
        PathBuf::from("/etc/grubstation/config.yaml")
    } else {
        if let Some(proj_dirs) = ProjectDirs::from("com", "grubstation", "grubstation") {
            proj_dirs.config_dir().join("config.yaml")
        } else {
            PathBuf::from("config.yaml")
        }
    }
}

pub fn load_config(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let s = ConfigLoader::builder()
        .set_default("daemon.port", DEFAULT_DAEMON_PORT)?
        .set_default("wake_on_lan.broadcast_address", DEFAULT_BROADCAST_ADDRESS.to_string())?
        .set_default("wake_on_lan.broadcast_port", DEFAULT_BROADCAST_PORT)?
        .add_source(File::from(path).format(FileFormat::Yaml))
        .build()?;

    let config: Config = s.try_deserialize()?;
    config.validate()?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_valid_config() -> Config {
        Config {
            host: HostConfig {
                interface: "lo".to_string(),
            },
            daemon: Some(DaemonConfig { port: DEFAULT_DAEMON_PORT }),
            wake_on_lan: Some(WakeOnLanConfig {
                broadcast_address: DEFAULT_BROADCAST_ADDRESS.to_string(),
                broadcast_port: DEFAULT_BROADCAST_PORT,
            }),
            grub: Some(GrubConfig {
                path: PathBuf::from("Cargo.toml"),
                network_wait: 10,
                webhook_id: "test-webhook-id".to_string(),
            }),
            webhook_id: None,
            api_key: None,
            ha_daemon_url: None,
            ha_grub_url: None,
        }
    }

    #[test]
    fn test_valid_config() {
        let config = create_valid_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_minimal_config() {
        let config = Config {
            host: HostConfig {
                interface: "lo".to_string(),
            },
            daemon: None,
            wake_on_lan: None,
            grub: None,
            webhook_id: None,
            api_key: None,
            ha_daemon_url: None,
            ha_grub_url: None,
        };
        assert!(config.validate().is_ok());

        // Verify that optional fields are skipped when serializing
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(!yaml.contains("webhook_id"));
        assert!(!yaml.contains("api_key"));
        assert!(!yaml.contains("ha_daemon_url"));
        assert!(!yaml.contains("ha_grub_url"));
        assert!(!yaml.contains("daemon"));
        assert!(!yaml.contains("wake_on_lan"));
        assert!(!yaml.contains("grub"));
    }

    #[test]
    fn test_empty_interface() {
        let mut config = create_valid_config();
        config.host.interface = "".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_port() {
        let mut config = create_valid_config();
        config.daemon.as_mut().unwrap().port = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_broadcast_address_v6() {
        let mut config = create_valid_config();
        config.wake_on_lan.as_mut().unwrap().broadcast_address = "::1".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_broadcast_address_malformed() {
        let mut config = create_valid_config();
        config.wake_on_lan.as_mut().unwrap().broadcast_address = "not-an-ip".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_grub_path() {
        let mut config = create_valid_config();
        config.grub.as_mut().unwrap().path = PathBuf::from("non_existent_file.txt");
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_network_wait() {
        let mut config = create_valid_config();
        config.grub.as_mut().unwrap().network_wait = 301;
        assert!(config.validate().is_err());
    }
}

