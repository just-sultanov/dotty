use dirs;
use std::{io, path};

/// Returns the path to the `$HOME/.dotty` directory.
pub fn root_path() -> io::Result<path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "The `home` directory was not found. Please set `$HOME`.",
        )
    })?;

    let root = home.join(".dotty");

    Ok(root)
}

/// Returns the path to the `$HOME/.dotty/.config/dotty/config.toml` file
pub fn config_path() -> io::Result<path::PathBuf> {
    let root = root_path()?;
    let config = root.join(".config/dotty/config.toml");
    Ok(config)
}
