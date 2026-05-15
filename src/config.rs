use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::DottyError;

/// Configuration stored in `config.toml` inside the state directory.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub machine: Option<String>,
    pub managed: IndexMap<String, String>,
}

impl Config {
    /// Create a new empty config.
    pub fn new() -> Self {
        Self {
            machine: None,
            managed: IndexMap::new(),
        }
    }

    /// Set the machine name.
    pub fn set_machine(&mut self, name: String) {
        self.machine = Some(name);
    }
}

/// Read `config.toml` from the state directory.
///
/// Returns a default (empty) config if the file doesn't exist.
pub fn read_config(state_path: &std::path::Path) -> Result<Config, DottyError> {
    let config_path = state_path.join("config.toml");
    if !config_path.exists() {
        return Ok(Config::new());
    }

    let content = std::fs::read_to_string(&config_path)?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}

/// Write `config.toml` to the state directory.
///
/// Creates the state directory if it doesn't exist.
pub fn write_config(state_path: &std::path::Path, config: &Config) -> Result<(), DottyError> {
    std::fs::create_dir_all(state_path)?;
    let content = toml::to_string_pretty(config)?;
    std::fs::write(state_path.join("config.toml"), content)?;
    Ok(())
}
