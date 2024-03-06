use std::{io, path};

use dirs;

const DOTTY_ROOT_PATH: &str = ".dotty";
const DOTTY_CONFIG_PATH: &str = ".config/dotty";
const DOTTY_CONFIG_FILENAME: &str = "config.toml";

/// Returns the path to the `$HOME/.dotty` directory.
pub fn root_path() -> io::Result<path::PathBuf> {
    let path = dirs::home_dir()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "The `home` directory was not found. Please set `$HOME`.",
            )
        })?
        .join(DOTTY_ROOT_PATH);

    Ok(path)
}

/// Returns the path to the `$HOME/.dotty/.config/dotty` directory.
pub fn config_path() -> io::Result<path::PathBuf> {
    let path = root_path()?.join(DOTTY_CONFIG_PATH);
    Ok(path)
}

/// Returns the path to the `$HOME/.dotty/.config/dotty/config.toml` file.
pub fn config_file_path() -> io::Result<path::PathBuf> {
    let path = config_path()?.join(DOTTY_CONFIG_FILENAME);
    Ok(path)
}
