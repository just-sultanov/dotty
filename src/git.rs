use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tracing::debug;

use crate::error::DottyError;
use crate::repo_state::RepoState;

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
pub(crate) fn git_run(dir: &Path, args: &[&str]) -> Result<String, DottyError> {
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

/// Commit staged changes with the given message.
///
/// Checks git identity (cached in `repo_state`) before attempting the commit.
pub(crate) fn git_commit(repo_state: &mut RepoState, message: &str) -> Result<(), DottyError> {
    // Pre-flight: check cached identity, fail fast if missing.
    repo_state.validate_git_identity()?;
    git_run(&repo_state.repo_path, &["commit", "-m", message])?;
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
    use crate::repo_state::RepoState;

    /// Pre-flight check helper for tests: verify git identity.
    ///
    /// This is the same logic as the production identity check, kept here
    /// as a test helper since the production path now uses RepoState.
    fn git_check_identity(dir: &std::path::Path) -> Result<(), crate::error::DottyError> {
        let name = crate::git::git_run(dir, &["config", "user.name"]);
        let email = crate::git::git_run(dir, &["config", "user.email"]);

        match (name, email) {
            (Ok(n), Ok(e)) if !n.trim().is_empty() && !e.trim().is_empty() => Ok(()),
            _ => Err(crate::error::DottyError::Git {
                exit_code: 127,
                stderr: "Git identity is not configured. Run `git config user.name 'Your Name'` and `git config user.email 'you@example.com'`".into(),
            }),
        }
    }
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
        let mut repo_state =
            RepoState::new_for_git(repo.path().to_path_buf(), repo.path().to_path_buf());
        let result = git_commit(&mut repo_state, "test commit");
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

    /// Test that git identity validation is cached after first check.
    ///
    /// Verifies that `validate_git_identity` returns Ok immediately on
    /// second call without spawning subprocesses (checked by verifying
    /// `git_identity_valid` is true after first call).
    #[test]
    fn test_git_identity_validation_is_cached() {
        let repo = create_repo_without_identity();
        // Set valid local identity
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

        let mut repo_state =
            RepoState::new_for_git(repo.path().to_path_buf(), repo.path().to_path_buf());
        assert!(!repo_state.git_identity_valid);

        // First call: should validate and cache
        assert!(repo_state.validate_git_identity().is_ok());
        assert!(repo_state.git_identity_valid);

        // Second call: should return Ok immediately (cached)
        assert!(repo_state.validate_git_identity().is_ok());
        assert!(repo_state.git_identity_valid);
    }

    /// Test that reset_git_identity allows re-checking after invalidation.
    #[test]
    fn test_reset_git_identity_allows_recheck() {
        let repo = create_repo_without_identity();
        // Set valid local identity
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

        let mut repo_state =
            RepoState::new_for_git(repo.path().to_path_buf(), repo.path().to_path_buf());

        // Validate and cache
        assert!(repo_state.validate_git_identity().is_ok());
        assert!(repo_state.git_identity_valid);

        // Reset the cache
        repo_state.reset_git_identity();
        assert!(!repo_state.git_identity_valid);

        // Re-validate should work again
        assert!(repo_state.validate_git_identity().is_ok());
        assert!(repo_state.git_identity_valid);
    }

    /// Test that cached identity check fails fast when identity is invalid.
    #[test]
    fn test_cached_identity_check_fails_when_invalid() {
        let repo = create_repo_without_identity();
        let mut repo_state =
            RepoState::new_for_git(repo.path().to_path_buf(), repo.path().to_path_buf());

        // First call: should fail (no identity)
        let result = repo_state.validate_git_identity();
        assert!(result.is_err());
        // Should still be false (not cached as valid)
        assert!(!repo_state.git_identity_valid);

        // Second call: should fail again (not cached, need to re-check)
        let result = repo_state.validate_git_identity();
        assert!(result.is_err());
        assert!(!repo_state.git_identity_valid);
    }
}
