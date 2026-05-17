use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::debug;

use crate::error::DottyError;

/// Run a git command in the given directory.
///
/// Returns the stdout as a string. On failure, returns a `DottyError::Git`
/// containing the stderr output.
fn git_run(dir: &Path, args: &[&str]) -> Result<String, DottyError> {
    debug!("git {}", args.join(" "));
    // Check for NotFound separately so users see "git is not installed" instead of "exit code -1".
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
pub(crate) fn git_commit(dir: &Path, message: &str) -> Result<(), DottyError> {
    git_run(dir, &["commit", "-m", message])?;
    Ok(())
}

/// List all tracked files in the repository (one per line).
pub(crate) fn git_ls_files(dir: &Path) -> Result<Vec<String>, DottyError> {
    let output = git_run(dir, &["ls-files"])?;
    Ok(output
        .lines()
        .filter(|l| !l.is_empty())
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
