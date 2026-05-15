use std::path::PathBuf;
use thiserror::Error;

/// Top-level error type for dotty.
///
/// Use `thiserror` for domain-specific errors and `anyhow` for the error chain
/// in `main()` and command dispatch.
#[derive(Error, Debug)]
pub(crate) enum DottyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("permission denied: {path}. Check file ownership or run with appropriate privileges.")]
    PermissionDenied { path: PathBuf },

    #[error("git command failed (exit code {exit_code}): {stderr}")]
    Git { exit_code: i32, stderr: String },

    #[error("config error: {0}")]
    Config(String),

    #[error("path error: {0}")]
    Path(String),

    #[error(
        "circular symlink detected: {path}. This symlink points to itself (directly or indirectly). Remove it manually or fix the target."
    )]
    CircularSymlink { path: PathBuf },

    #[error("prompt error: {0}")]
    Prompt(#[from] dialoguer::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("backup verification failed for {path}: {detail}")]
    BackupVerification { path: PathBuf, detail: String },

    #[error("user cancelled operation")]
    Cancelled,
}
