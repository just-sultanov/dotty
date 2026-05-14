use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::DottyError;

/// Run a git command in the given directory.
///
/// Returns the stdout as a string. On failure, returns a `DottyError::Git`
/// containing the stderr output.
fn git_run(dir: &Path, args: &[&str]) -> Result<String, DottyError> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| DottyError::Git(format!("failed to execute git: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(DottyError::Git(format!(
            "git {} failed: {stderr}",
            args.join(" ")
        )));
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
    let parent = dir.parent().ok_or_else(|| {
        DottyError::Path(format!("cannot determine parent of: {}", dir.display()))
    })?;

    // Prevent cloning into the root directory
    if parent.as_os_str().is_empty() || parent == Path::new("/") {
        return Err(DottyError::Path(
            "cannot clone into the root directory".to_string(),
        ));
    }

    git_run(
        parent,
        &[
            "clone",
            url,
            dir.to_str()
                .ok_or_else(|| DottyError::Path(format!("invalid path: {}", dir.display())))?,
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
/// Stage files in the repository.
pub fn git_add(dir: &Path, paths: &[PathBuf]) -> Result<(), DottyError> {
    let path_args: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
    let mut args = vec!["add"];
    args.extend(path_args);
    git_run(dir, &args)?;
    Ok(())
}

#[allow(dead_code)]
/// Commit staged changes with the given message.
pub fn git_commit(dir: &Path, message: &str) -> Result<(), DottyError> {
    git_run(dir, &["commit", "-m", message])?;
    Ok(())
}

#[allow(dead_code)]
/// List all tracked files in the repository (one per line).
pub fn git_ls_files(dir: &Path) -> Result<Vec<String>, DottyError> {
    let output = git_run(dir, &["ls-files"])?;
    Ok(output
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

#[allow(dead_code)]
/// Get the git status summary (porcelain format).
pub fn git_status(dir: &Path) -> Result<String, DottyError> {
    git_run(dir, &["status", "--porcelain"])
}

#[allow(dead_code)]
/// Get the current branch name.
pub fn git_current_branch(dir: &Path) -> Result<String, DottyError> {
    git_run(dir, &["branch", "--show-current"]).map(|s| s.trim().to_string())
}

#[allow(dead_code)]
/// Reset staged files (unstage).
pub fn git_reset(dir: &Path, paths: &[&str]) -> Result<(), DottyError> {
    let mut args = vec!["reset", "HEAD"];
    args.extend_from_slice(paths);
    git_run(dir, &args)?;
    Ok(())
}

#[allow(dead_code)]
/// Soft reset to undo the last commit.
pub fn git_reset_soft_head(dir: &Path) -> Result<(), DottyError> {
    git_run(dir, &["reset", "--soft", "HEAD~1"])?;
    Ok(())
}
