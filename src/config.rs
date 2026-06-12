use config::{Config as ConfigLoader, File, FileFormat};
use directories::ProjectDirs;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use validator::Validate;
use std::str::FromStr;
use std::net::IpAddr;
use mac_address::MacAddress;

#[derive(Debug, Deserialize, Validate)]
pub struct Config {
    #[validate(nested)]
    pub host: HostConfig,
    #[validate(nested)]
    pub daemon: DaemonConfig,
}

#[derive(Debug, Deserialize, Validate)]
pub struct HostConfig {
    #[validate(custom(function = "validate_mac_address"))]
    pub mac: String,
    #[validate(custom(function = "validate_address"))]
    pub address: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct DaemonConfig {
    #[validate(range(min = 1025, max = 65535))]
    pub port: u16,
}

fn validate_mac_address(mac: &str) -> Result<(), validator::ValidationError> {
    if MacAddress::from_str(mac).is_ok() {
        Ok(())
    } else {
        Err(validator::ValidationError::new("invalid_mac"))
    }
}

fn validate_address(address: &str) -> Result<(), validator::ValidationError> {
    let is_ip = IpAddr::from_str(address).is_ok();
    // Simple hostname/domain validation: alphanumeric, dots, and dashes, and not empty.
    let is_hostname = !address.is_empty() && address.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-');
    
    if is_ip || is_hostname {
        Ok(())
    } else {
        Err(validator::ValidationError::new("invalid_address"))
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
        .add_source(File::from(path).format(FileFormat::Yaml))
        .build()?;

    let config: Config = s.try_deserialize()?;
    config.validate()?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_config() {
        let config = Config {
            host: HostConfig {
                mac: "00:11:22:33:44:55".to_string(),
                address: "127.0.0.1".to_string(),
            },
            daemon: DaemonConfig { port: 8081 },
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_port() {
        let config = DaemonConfig { port: 0 };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_valid_port() {
        let config = DaemonConfig { port: 65535 };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_mac() {
        let config = HostConfig {
            mac: "invalid".to_string(),
            address: "127.0.0.1".to_string(),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_address() {
        let config = HostConfig {
            mac: "00:11:22:33:44:55".to_string(),
            address: "not valid!".to_string(),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_valid_ip_v4() {
        let config = HostConfig {
            mac: "00:11:22:33:44:55".to_string(),
            address: "192.168.1.1".to_string(),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_valid_ip_v6() {
        let config = HostConfig {
            mac: "00:11:22:33:44:55".to_string(),
            address: "::1".to_string(),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_valid_hostname() {
        let config = HostConfig {
            mac: "00:11:22:33:44:55".to_string(),
            address: "grubstation.local".to_string(),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_empty_address() {
        let config = HostConfig {
            mac: "00:11:22:33:44:55".to_string(),
            address: "".to_string(),
        };
        assert!(config.validate().is_err());
    }
}
