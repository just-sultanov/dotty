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

    #[error("invalid machine name '{name}': {reason}")]
    InvalidMachineName { name: String, reason: String },

    #[error("cannot determine home directory: {0}")]
    MissingHomeDirectory(String),

    #[error("invalid repo path '{path}': {reason}")]
    InvalidRepoPath { path: String, reason: String },

    #[error("cannot map target path '{path}': {reason}")]
    InvalidTargetPath { path: String, reason: String },

    #[error("path resolution error: {reason}")]
    PathResolution { path: PathBuf, reason: String },

    #[error(
        "circular symlink detected: {path}. This symlink points to itself (directly or indirectly). Remove it manually or fix the target."
    )]
    CircularSymlink { path: PathBuf },

    #[error("prompt error: {0}")]
    Prompt(#[from] dialoguer::Error),

    #[error("not running in an interactive terminal: {hint}")]
    NotInteractive { hint: String },

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("backup verification failed for {path}: {detail}")]
    BackupVerification { path: PathBuf, detail: String },

    #[error("no dotty repository found at {path}. Run `dotty init` first.")]
    MissingGitRepository { path: PathBuf },

    #[allow(dead_code)]
    #[error("no machine configured. Run `dotty config machine <name>` to set one.")]
    MissingMachineName,

    #[error("user cancelled operation")]
    Cancelled,
}
