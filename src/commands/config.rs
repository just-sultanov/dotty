use anyhow::Result;

use crate::convention::{read_config, resolve_state_path, validate_machine_name, write_config};

/// Set the current machine name.
///
/// Writes the machine name to `config.toml` in the state directory.
pub fn set_machine(name: String) -> Result<()> {
    validate_machine_name(&name)?;

    let state_path = resolve_state_path()?;

    let mut config = read_config(&state_path)?;
    config.set_machine(name.clone());

    write_config(&state_path, &config)?;

    println!("Machine set to: {}", name);
    Ok(())
}
