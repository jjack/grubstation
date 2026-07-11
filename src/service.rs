use anyhow::Result;
use std::path::Path;

#[cfg(target_os = "linux")]
pub fn install_service(config_path: &Path) -> Result<()> {
    let exe_path = std::env::current_exe()?;
    let abs_config_path = config_path.canonicalize()?;

    let service_content = include_str!("../templates/grubstation.service")
        .replace("{{EXE_PATH}}", &exe_path.to_string_lossy())
        .replace("{{CONFIG_PATH}}", &abs_config_path.to_string_lossy());

    let service_file_path = Path::new("/etc/systemd/system/grubstation.service");
    if let Some(parent) = service_file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(service_file_path, service_content)?;

    // Reload systemd daemon
    let status = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status()?;
    if !status.success() {
        anyhow::bail!("systemctl daemon-reload failed");
    }

    // Enable service
    let status = std::process::Command::new("systemctl")
        .arg("enable")
        .arg("grubstation.service")
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to enable grubstation service");
    }

    Ok(())
}

#[cfg(target_os = "linux")]
pub fn start_service() -> Result<()> {
    // Start service
    let status = std::process::Command::new("systemctl")
        .arg("start")
        .arg("grubstation.service")
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to start grubstation service");
    }

    Ok(())
}

#[cfg(target_os = "windows")]
pub fn install_service(config_path: &Path) -> Result<()> {
    let exe_path = std::env::current_exe()?;
    let abs_config_path = config_path.canonicalize()?;

    let bin_path = format!(
        "\"{}\" --config \"{}\" run",
        exe_path.display(),
        abs_config_path.display()
    );

    // Run sc create
    let status = std::process::Command::new("sc")
        .args(&[
            "create",
            "grubstation",
            &format!("binPath= {}", bin_path),
            "start= auto",
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to create Windows service");
    }

    Ok(())
}

#[cfg(target_os = "windows")]
pub fn start_service() -> Result<()> {
    // Run sc start
    let status = std::process::Command::new("sc")
        .args(&["start", "grubstation"])
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to start Windows service");
    }

    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn install_service(_config_path: &Path) -> Result<()> {
    anyhow::bail!("Unsupported platform for service installation");
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn start_service() -> Result<()> {
    anyhow::bail!("Unsupported platform for starting service");
}

#[cfg(target_os = "linux")]
pub fn install_shutdown_hook(config_path: &Path) -> Result<()> {
    let exe_path = std::env::current_exe()?;
    let abs_config_path = config_path.canonicalize()?;

    let service_content = include_str!("../templates/grubstation-shutdown.service")
        .replace("{{EXE_PATH}}", &exe_path.to_string_lossy())
        .replace("{{CONFIG_PATH}}", &abs_config_path.to_string_lossy());

    let service_file_path = Path::new("/etc/systemd/system/grubstation-shutdown.service");
    if let Some(parent) = service_file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(service_file_path, service_content)?;

    // Reload systemd daemon
    let status = std::process::Command::new("systemctl").arg("daemon-reload").status()?;
    if !status.success() {
        anyhow::bail!("systemctl daemon-reload failed");
    }

    // Enable service
    let status = std::process::Command::new("systemctl")
        .arg("enable")
        .arg("grubstation-shutdown.service")
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to enable grubstation-shutdown service");
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn install_shutdown_hook(_config_path: &Path) -> Result<()> {
    anyhow::bail!("Shutdown hooks are only supported on Linux via systemd");
}
