use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use dirs;

/// Returns the path to the `$HOME/.dotty` directory.
pub fn dotty_home() -> Result<PathBuf> {
    let path = match env::var_os("DOTTY_HOME") {
        Some(path) => PathBuf::from(path),
        None => dirs::home_dir()
            .context("The `home` directory was not found. Please set `$HOME`.")?
            .join(".dotty"),
    };

    Ok(path)
}
