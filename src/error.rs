#![allow(dead_code)]

use thiserror::Error;

/// Top-level error type for dotty.
///
/// Use `thiserror` for domain-specific errors and `anyhow` for the error chain
/// in `main()` and command dispatch.
#[derive(Error, Debug)]
pub enum DottyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("git command failed: {0}")]
    Git(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("path error: {0}")]
    Path(String),

    #[error("symlink error: {0}")]
    Symlink(String),

    #[error("prompt error: {0}")]
    Prompt(#[from] dialoguer::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("user cancelled operation")]
    Cancelled,
}
