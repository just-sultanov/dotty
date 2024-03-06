use std::{io, path};

use dirs;

const DOTTY_ROOT_PATH: &str = ".dotty";
const DOTTY_CONFIG_PATH: &str = ".config/dotty";
const DOTTY_CONFIG_FILENAME: &str = "config.toml";

/// Returns the path to the `$HOME/.dotty` directory.
pub fn root_path() -> io::Result<path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "The `home` directory was not found. Please set `$HOME`.",
        )
    })?;

    let root = home.join(DOTTY_ROOT_PATH);

    Ok(root)
}

/// Returns the path to the `$HOME/.dotty/.config/dotty` directory.
pub fn config_path() -> io::Result<path::PathBuf> {
    let root = root_path()?;
    let config = root.join(DOTTY_CONFIG_PATH);
    Ok(config)
}

/// Returns the path to the `$HOME/.dotty/.config/dotty/config.toml` file.
pub fn config_file_path() -> io::Result<path::PathBuf> {
    let root = config_path()?;
    let config = root.join(DOTTY_CONFIG_FILENAME);
    Ok(config)
}
