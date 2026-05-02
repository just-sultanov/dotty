use anyhow::Result;

use crate::convention::{read_config, resolve_state_path, write_config};

/// Set the current machine name.
///
/// Writes the machine name to `config.toml` in the state directory.
pub fn set_machine(name: String) -> Result<()> {
    let state_path = resolve_state_path().map_err(|e| anyhow::anyhow!(e))?;

    let mut config = read_config(&state_path).map_err(|e| anyhow::anyhow!(e))?;
    config.set_machine(name.clone());

    write_config(&state_path, &config).map_err(|e| anyhow::anyhow!(e))?;

    println!("Machine set to: {}", name);
    Ok(())
}
