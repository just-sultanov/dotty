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
    let tmp_path = state_path.join("config.toml.tmp");
    if tmp_path.exists() {
        tracing::warn!("removing stale temp file: {}", tmp_path.display());
        std::fs::remove_file(&tmp_path)?;
    }

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
/// Uses an atomic write pattern (write to temp file, then rename) to prevent
/// config.toml corruption if the process is killed mid-write. A partial TOML
/// write would leave the file unreadable, causing all subsequent dotty commands
/// to fail with a parse error.
///
/// `fs::rename` is atomic on POSIX when source and dest are on the same
/// filesystem — guaranteed here since the temp file lives in the same directory.
///
/// Creates the state directory if it doesn't exist.
pub fn write_config(state_path: &std::path::Path, config: &Config) -> Result<(), DottyError> {
    std::fs::create_dir_all(state_path)?;
    let content = toml::to_string_pretty(config)?;
    // Write to a temp file first, then atomically rename into place.
    let tmp_path = state_path.join("config.toml.tmp");
    std::fs::write(&tmp_path, &content)?;
    let dest_path = state_path.join("config.toml");
    std::fs::rename(&tmp_path, dest_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_atomic_write_config_produces_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path();

        let mut config = Config::new();
        config.set_machine("test-machine".to_string());

        write_config(state_path, &config).unwrap();

        // Verify the file exists and contains valid TOML.
        let config_path = state_path.join("config.toml");
        assert!(config_path.exists());

        let content = fs::read_to_string(&config_path).unwrap();
        let parsed: Config = toml::from_str(&content).unwrap();
        assert_eq!(parsed.machine, Some("test-machine".to_string()));

        // Verify no temp file is left behind after successful rename.
        let tmp_path = state_path.join("config.toml.tmp");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn test_write_config_creates_directory_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("nested").join("state");

        let config = Config::new();
        write_config(&state_path, &config).unwrap();

        let config_path = state_path.join("config.toml");
        assert!(config_path.exists());
    }

    #[test]
    fn test_read_config_removes_stale_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path();

        let tmp_path = state_path.join("config.toml.tmp");
        std::fs::write(&tmp_path, b"garbage").unwrap();
        assert!(tmp_path.exists());

        let config = read_config(state_path).unwrap();
        assert!(!tmp_path.exists());
        assert!(config.machine.is_none());
    }
}
