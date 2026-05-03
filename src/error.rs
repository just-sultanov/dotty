use thiserror::Error;

/// Top-level error type for dotty.
///
/// Use `thiserror` for domain-specific errors and `anyhow` for the error chain
/// in `main()` and command dispatch.
#[allow(dead_code)]
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

    #[error("TOML error: {0}")]
    Toml(String),

    #[error("user cancelled operation")]
    Cancelled,
}
