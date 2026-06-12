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

    // TODO: Implement the rest of the wizard questions
    println!("Continuing with initialization...");

    outro("Configuration initialized successfully.")?;
    Ok(())
}
