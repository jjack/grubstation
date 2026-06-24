use config::{Config as ConfigLoader, File, FileFormat};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use validator::Validate;
use std::str::FromStr;
use std::net::IpAddr;
use mac_address::MacAddress;

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
    #[validate(nested)]
    pub daemon: Option<DaemonConfig>,
    #[validate(nested)]
    pub wake_on_lan: Option<WakeOnLanConfig>,
    #[validate(nested)]
    pub grub: Option<GrubConfig>,
    #[serde(default)]
    pub webhook_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub ha_daemon_url: Option<String>,
    #[serde(default)]
    pub ha_grub_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone)]
pub struct HostConfig {
    #[validate(custom(function = "validate_mac_address"))]
    pub mac: String,
    #[validate(custom(function = "validate_address"))]
    pub address: String,
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
    #[serde(default)]
    pub webhook_id: String,
}

fn default_network_wait() -> u32 {
    10
}

fn validate_mac_address(mac: &str) -> Result<(), validator::ValidationError> {
    if MacAddress::from_str(mac).is_ok() {
        Ok(())
    } else {
        Err(validator::ValidationError::new("invalid_mac"))
    }
}

fn validate_address(address: &str) -> Result<(), validator::ValidationError> {
    let is_ipv4 = validate_ipv4(address).is_ok();
    // Simple hostname/domain validation: alphanumeric, dots, and dashes, and not empty.
    let is_hostname = !address.is_empty() && address.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-');
    
    if is_ipv4 || is_hostname {
        Ok(())
    } else {
        Err(validator::ValidationError::new("invalid_address"))
    }
}

fn validate_ipv4(address: &str) -> Result<(), validator::ValidationError> {
    match IpAddr::from_str(address) {
        Ok(IpAddr::V4(_)) => Ok(()),
        _ => Err(validator::ValidationError::new("invalid_ipv4")),
    }
}

fn validate_file_exists(path: &Path) -> Result<(), validator::ValidationError> {
    if path.exists() && path.is_file() {
        Ok(())
    } else {
        Err(validator::ValidationError::new("file_not_found"))
    }
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
                mac: "00:11:22:33:44:55".to_string(),
                address: "127.0.0.1".to_string(),
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
                mac: "00:11:22:33:44:55".to_string(),
                address: "127.0.0.1".to_string(),
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
    }

    #[test]
    fn test_invalid_mac() {
        let mut config = create_valid_config();
        config.host.mac = "invalid".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_address() {
        let mut config = create_valid_config();
        config.host.address = "not valid!".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_ip_v6() {
        let mut config = create_valid_config();
        config.host.address = "::1".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_empty_address() {
        let mut config = create_valid_config();
        config.host.address = "".to_string();
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

