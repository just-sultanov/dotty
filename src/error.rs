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

    #[error(
        "invalid machine name '{name}': {reason}. Use alphanumeric characters and hyphens (e.g. 'my-laptop')."
    )]
    InvalidMachineName { name: String, reason: String },

    #[error("cannot determine home directory: {0}. Set the HOME environment variable.")]
    MissingHomeDirectory(String),

    #[error(
        "invalid repo path '{path}': {reason}. Ensure path follows convention: <scope>/<root>/... (e.g. 'home/.bashrc')."
    )]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_machine_name_message() {
        let err = DottyError::InvalidMachineName {
            name: "my machine!".into(),
            reason: "contains spaces".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("'my machine!'"));
        assert!(msg.contains("contains spaces"));
        assert!(msg.contains("alphanumeric characters and hyphens"));
        assert!(msg.contains("'my-laptop'"));
    }

    #[test]
    fn test_missing_home_directory_message() {
        let err = DottyError::MissingHomeDirectory("no HOME set".into());
        let msg = err.to_string();
        assert!(msg.contains("no HOME set"));
        assert!(msg.contains("Set the HOME environment variable"));
    }

    #[test]
    fn test_invalid_repo_path_message() {
        let err = DottyError::InvalidRepoPath {
            path: ".bashrc".into(),
            reason: "missing scope prefix".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("'.bashrc'"));
        assert!(msg.contains("missing scope prefix"));
        assert!(msg.contains("<scope>/<root>/..."));
        assert!(msg.contains("'home/.bashrc'"));
    }

    #[test]
    fn test_permission_denied_message() {
        let err = DottyError::PermissionDenied {
            path: PathBuf::from("/etc/shadow"),
        };
        let msg = err.to_string();
        assert!(msg.contains("/etc/shadow"));
        assert!(msg.contains("Check file ownership"));
    }

    #[test]
    fn test_git_error_message() {
        let err = DottyError::Git {
            exit_code: 128,
            stderr: "fatal: not a git repository".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("128"));
        assert!(msg.contains("fatal: not a git repository"));
    }

    #[test]
    fn test_circular_symlink_message() {
        let err = DottyError::CircularSymlink {
            path: PathBuf::from("/home/user/.config/link"),
        };
        let msg = err.to_string();
        assert!(msg.contains("/home/user/.config/link"));
        assert!(msg.contains("circular symlink"));
        assert!(msg.contains("Remove it manually"));
    }

    #[test]
    fn test_missing_git_repository_message() {
        let err = DottyError::MissingGitRepository {
            path: PathBuf::from("/home/user/dotfiles"),
        };
        let msg = err.to_string();
        assert!(msg.contains("/home/user/dotfiles"));
        assert!(msg.contains("dotty init"));
    }

    #[test]
    fn test_missing_machine_name_message() {
        let err = DottyError::MissingMachineName;
        let msg = err.to_string();
        assert!(msg.contains("dotty config machine"));
    }

    #[test]
    fn test_cancelled_message() {
        let err = DottyError::Cancelled;
        assert_eq!(err.to_string(), "user cancelled operation");
    }

    #[test]
    fn test_not_interactive_message() {
        let err = DottyError::NotInteractive {
            hint: "use --force to bypass".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("not running in an interactive terminal"));
        assert!(msg.contains("use --force to bypass"));
    }

    #[test]
    fn test_backup_verification_message() {
        let err = DottyError::BackupVerification {
            path: PathBuf::from("/home/user/.bashrc.bak"),
            detail: "checksum mismatch".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("/home/user/.bashrc.bak"));
        assert!(msg.contains("checksum mismatch"));
    }

    #[test]
    fn test_path_resolution_message() {
        let err = DottyError::PathResolution {
            path: PathBuf::from("/some/path"),
            reason: "component not found".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("path resolution error"));
        assert!(msg.contains("component not found"));
    }

    #[test]
    fn test_invalid_target_path_message() {
        let err = DottyError::InvalidTargetPath {
            path: "/tmp/test".into(),
            reason: "outside home directory".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("/tmp/test"));
        assert!(msg.contains("outside home directory"));
    }
}
