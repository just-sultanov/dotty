use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tracing::debug;

use crate::error::DottyError;

/// Run a git command in the given directory.
///
/// Returns the raw `std::process::Output` (stdout, stderr, exit code).
/// On failure to execute git (e.g., not installed), returns a `DottyError::GitNotInstalled`.
///
/// This is the low-level primitive. Callers can inspect `output.status.success()`
/// and `output.stderr` to produce domain-specific errors (e.g. `PendingPlanInvalid`
/// for a corrupted repo vs `Git` for a command failure).
pub(crate) fn git_run_raw(dir: &Path, args: &[&str]) -> Result<Output, DottyError> {
    debug!("git {}", args.join(" "));
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                DottyError::GitNotInstalled { source: e }
            } else {
                DottyError::Git {
                    exit_code: -1,
                    stderr: format!("failed to execute git: {e}"),
                }
            }
        })?;
    Ok(output)
}

/// Run a git command in the given directory.
///
/// Returns the stdout as a string. On failure, returns a `DottyError::Git`
/// containing the stderr output.
fn git_run(dir: &Path, args: &[&str]) -> Result<String, DottyError> {
    let output = git_run_raw(dir, args)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        return Err(DottyError::Git {
            exit_code,
            stderr: format!("git {} failed: {stderr}", args.join(" ")),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Initialize a new git repository in the given directory.
pub fn git_init(dir: &Path) -> Result<(), DottyError> {
    git_run(dir, &["init"])?;
    Ok(())
}

/// Clone a repository into the given directory.
pub fn git_clone(url: &str, dir: &Path) -> Result<(), DottyError> {
    let parent = dir.parent().ok_or_else(|| DottyError::PathResolution {
        path: dir.to_path_buf(),
        reason: format!("cannot determine parent of: {}", dir.display()),
    })?;

    // Prevent cloning into the root directory
    if parent.as_os_str().is_empty() || parent == Path::new("/") {
        return Err(DottyError::PathResolution {
            path: dir.to_path_buf(),
            reason: "cannot clone into the root directory".into(),
        });
    }

    git_run(
        parent,
        &[
            "clone",
            url,
            dir.to_str().ok_or_else(|| DottyError::PathResolution {
                path: dir.to_path_buf(),
                reason: format!("path is not valid UTF-8: {}", dir.display()),
            })?,
        ],
    )?;
    Ok(())
}

/// Stage files in the repository.
pub(crate) fn git_add(dir: &Path, paths: &[PathBuf]) -> Result<(), DottyError> {
    let path_args: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
    let mut args = vec!["add"];
    args.extend(path_args);
    git_run(dir, &args)?;
    Ok(())
}

/// Pre-flight check: verify git identity (user.name / user.email) is configured.
///
/// Returns a descriptive error with actionable guidance if either setting is missing.
/// This prevents `git commit` from failing with exit code 127 on fresh machines.
fn git_check_identity(dir: &Path) -> Result<(), DottyError> {
    let name = git_run(dir, &["config", "user.name"]);
    let email = git_run(dir, &["config", "user.email"]);

    match (name, email) {
        (Ok(n), Ok(e)) if !n.trim().is_empty() && !e.trim().is_empty() => Ok(()),
        _ => Err(DottyError::Git {
            exit_code: 127,
            stderr: "Git identity is not configured. Run `git config user.name 'Your Name'` and `git config user.email 'you@example.com'`".into(),
        }),
    }
}

/// Commit staged changes with the given message.
pub(crate) fn git_commit(dir: &Path, message: &str) -> Result<(), DottyError> {
    // Pre-flight: fail fast if git identity is missing, before attempting the commit.
    git_check_identity(dir)?;
    git_run(dir, &["commit", "-m", message])?;
    Ok(())
}

/// List all tracked files in the repository.
///
/// Uses `-z` for null-delimited output so that filenames containing spaces,
/// newlines, or non-ASCII characters are parsed correctly without git's quoted
/// output mode interfering.
pub(crate) fn git_ls_files(dir: &Path) -> Result<Vec<String>, DottyError> {
    let output = git_run(dir, &["ls-files", "-z"])?;
    Ok(output
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

/// Get the git status summary (porcelain format).
pub(crate) fn git_status(dir: &Path) -> Result<String, DottyError> {
    git_run(dir, &["status", "--porcelain"])
}

/// Get the current branch name.
pub(crate) fn git_current_branch(dir: &Path) -> Result<String, DottyError> {
    git_run(dir, &["branch", "--show-current"]).map(|s| s.trim().to_string())
}

/// Reset staged files (unstage).
pub(crate) fn git_reset(dir: &Path, paths: &[&str]) -> Result<(), DottyError> {
    let mut args = vec!["reset", "HEAD"];
    args.extend_from_slice(paths);
    git_run(dir, &args)?;
    Ok(())
}

/// Soft reset to undo the last commit.
pub(crate) fn git_reset_soft_head(dir: &Path) -> Result<(), DottyError> {
    git_run(dir, &["reset", "--soft", "HEAD~1"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    /// Helper: create a git repo with empty local identity to shadow global config.
    ///
    /// Local config takes precedence over global, so setting empty values ensures
    /// `git config user.name` returns empty regardless of global settings.
    fn create_repo_without_identity() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["init"])
            .output()
            .unwrap();
        // Set empty local values to shadow any global config
        Command::new("git")
            .current_dir(dir.path())
            .args(["config", "--local", "user.name", ""])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["config", "--local", "user.email", ""])
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn git_check_identity_rejects_missing_identity() {
        let repo = create_repo_without_identity();
        let result = git_check_identity(repo.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            DottyError::Git { exit_code, stderr } => {
                assert_eq!(exit_code, 127);
                assert!(stderr.contains("Git identity is not configured"));
                assert!(stderr.contains("git config user.name"));
                assert!(stderr.contains("git config user.email"));
            }
            _ => panic!("expected DottyError::Git, got {err:?}"),
        }
    }

    #[test]
    fn git_check_identity_accepts_valid_identity() {
        let repo = create_repo_without_identity();
        // Override with valid local identity
        Command::new("git")
            .current_dir(repo.path())
            .args(["config", "--local", "user.name", "Test User"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(repo.path())
            .args(["config", "--local", "user.email", "test@example.com"])
            .output()
            .unwrap();
        assert!(git_check_identity(repo.path()).is_ok());
    }

    #[test]
    fn git_ls_files_handles_special_characters() {
        let repo = tempdir().unwrap();
        // Initialize repo
        Command::new("git")
            .current_dir(repo.path())
            .args(["init"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(repo.path())
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(repo.path())
            .args(["config", "user.email", "test@test.com"])
            .output()
            .unwrap();

        // Create files with special characters: spaces, non-ASCII
        fs::write(repo.path().join("my file.txt"), "spaces").unwrap();
        fs::write(repo.path().join("café.txt"), "non-ascii").unwrap();
        fs::write(repo.path().join("normal.txt"), "normal").unwrap();

        // Stage and commit
        Command::new("git")
            .current_dir(repo.path())
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(repo.path())
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();

        let files = git_ls_files(repo.path()).unwrap();
        assert!(files.contains(&"my file.txt".to_string()));
        assert!(files.contains(&"café.txt".to_string()));
        assert!(files.contains(&"normal.txt".to_string()));
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn git_commit_fails_before_commit_when_identity_missing() {
        let repo = create_repo_without_identity();
        // Create a file so there's something to commit
        fs::write(repo.path().join("test.txt"), "hello").unwrap();
        Command::new("git")
            .current_dir(repo.path())
            .args(["add", "test.txt"])
            .output()
            .unwrap();
        let result = git_commit(repo.path(), "test commit");
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            DottyError::Git { exit_code, stderr } => {
                assert_eq!(exit_code, 127);
                assert!(stderr.contains("Git identity is not configured"));
            }
            _ => panic!("expected DottyError::Git, got {err:?}"),
        }
    }
}
