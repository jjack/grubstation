use directories::ProjectDirs;
use mac_address::MacAddress;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub host: HostConfig,
}

#[derive(Debug, Deserialize)]
pub struct HostConfig {
    pub mac: String,
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

pub fn validate_config(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    println!("Reading config from: {:?}", path);
    let content = fs::read_to_string(path)?;
    let config: Config = serde_yaml::from_str(&content)?;

    // Validate MAC address
    match MacAddress::from_str(&config.host.mac) {
        Ok(_) => {
            println!("Success: MAC address '{}' is valid.", config.host.mac);
            Ok(config)
        }
        Err(_) => Err(format!("Error: '{}' is not a valid MAC address.", config.host.mac).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_mac() {
        let content = "host:\n  mac: '00:11:22:33:44:55'";
        let config: Config = serde_yaml::from_str(content).unwrap();
        assert!(MacAddress::from_str(&config.host.mac).is_ok());
    }

    #[test]
    fn test_invalid_mac() {
        let content = "host:\n  mac: 'invalid-mac'";
        let config: Config = serde_yaml::from_str(content).unwrap();
        assert!(MacAddress::from_str(&config.host.mac).is_err());
    }

    #[test]
    fn test_empty_mac() {
        let content = "host:\n  mac: ''";
        let config: Config = serde_yaml::from_str(content).unwrap();
        assert!(MacAddress::from_str(&config.host.mac).is_err());
    }
}
