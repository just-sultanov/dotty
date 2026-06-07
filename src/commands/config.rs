use anyhow::Result;

use crate::config::{read_config, write_config};
use crate::convention::MachineName;
use crate::repo_state::RepoState;

/// Set the current machine name.
///
/// Writes the machine name to `config.toml` in the config directory.
pub fn set_machine(name: String) -> Result<()> {
    MachineName::new(&name)?;

    let repo = RepoState::new()?;
    let config_path = repo.config_path.clone();

    let mut config = read_config(&config_path)?;
    config.set_machine(name.clone());

    write_config(&config_path, &config)?;

    println!("Machine set to: {}", name);
    Ok(())
}
