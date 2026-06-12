use anyhow::Result;
use cliclack::{confirm, intro, outro};
use std::path::Path;

pub fn wizard_init(config_path: &Path) -> Result<()> {
    intro("Grubstation Configuration Wizard")?;

    if config_path.exists() {
        let should_overwrite = confirm(format!(
            "Config file already exists at {:?}. Overwrite?",
            config_path
        ))
        .initial_value(false)
        .interact()?;

        if !should_overwrite {
            outro("Initialization cancelled.")?;
            return Ok(());
        }
    }

    let mut sync_grub = false;
    if cfg!(target_os = "linux") {
        let grub_paths = ["/boot/grub/grub.cfg", "/boot/grub2/grub.cfg"];
        let found_grub = grub_paths.iter().find(|p| Path::new(p).exists());

        if let Some(path) = found_grub {
            sync_grub = confirm(format!(
                "Found GRUB config at {}. Sync boot options with Home Assistant?",
                path
            ))
            .initial_value(true)
            .interact()?;
        }
    }

    // TODO: Implement the rest of the wizard questions
    if sync_grub {
        println!("GRUB sync enabled.");
    }

    println!("Continuing with initialization...");

    outro("Configuration initialized successfully.")?;
    Ok(())
}
