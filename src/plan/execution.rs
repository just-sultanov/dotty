//! Plan execution and rollback logic.
//!
//! Contains [`execute_plan`], the [`RollbackAction`] enum, and helper functions
//! for executing individual actions, rolling back completed actions, and
//! copying/verifying files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{debug, trace, warn};

use indicatif::ProgressBar;

use crate::error::DottyError;
use crate::git;
use crate::symlink::{self, is_symlink};

use super::Action;

// ---------------------------------------------------------------------------
// Action execution and rollback (moved from plan.rs)
// ---------------------------------------------------------------------------

/// Execute the filesystem or git mutation described by this action.
///
/// `repo_path` is used as the working directory for git operations.
pub(crate) fn action_execute(action: &Action, repo_path: &Path) -> Result<(), DottyError> {
    match action {
        Action::CreateDir { path } => {
            fs::create_dir_all(path).map_err(|e| io_error_with_path(e, path))?;
        }
        Action::Backup { source, dest } => {
            let parent = dest.parent().ok_or_else(|| DottyError::PathResolution {
                path: dest.to_path_buf(),
                reason: format!("cannot determine parent of backup path: {}", dest.display()),
            })?;
            fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
            copy_file_dereference(source, dest)?;
            verify_backup_integrity(source, dest)?;
        }
        Action::CopyFile { source, dest } => {
            let parent = dest.parent();
            if let Some(p) = parent {
                fs::create_dir_all(p).map_err(|e| io_error_with_path(e, p))?;
            }
            copy_file_dereference(source, dest)?;
        }
        Action::CreateSymlink { target, link, .. } => {
            let parent = link.parent();
            if let Some(p) = parent {
                fs::create_dir_all(p).map_err(|e| io_error_with_path(e, p))?;
            }
            if symlink::would_be_circular(target, link) {
                return Err(DottyError::CircularSymlink { path: link.clone() });
            }
            if fs::symlink_metadata(link).is_ok() {
                if link.is_dir() && !crate::symlink::is_symlink(link) {
                    fs::remove_dir_all(link).map_err(|e| io_error_with_path(e, link))?;
                } else {
                    fs::remove_file(link).map_err(|e| io_error_with_path(e, link))?;
                }
            }
            crate::symlink::create_symlink(target, link)
                .map_err(|e| io_error_with_path(e, link))?;
        }
        Action::RemoveFile { path } => {
            if !path.exists() {
                return Ok(());
            }
            if path.is_dir() && !is_symlink(path) {
                fs::remove_dir_all(path).map_err(|e| io_error_with_path(e, path))?;
            } else {
                fs::remove_file(path).map_err(|e| io_error_with_path(e, path))?;
            }
        }
        Action::RemoveSymlink { path } => {
            if is_symlink(path) {
                fs::remove_file(path).map_err(|e| io_error_with_path(e, path))?;
            }
        }
        Action::RestoreBackup { source, dest } => {
            if is_symlink(dest) {
                fs::remove_file(dest).map_err(|e| io_error_with_path(e, dest))?;
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
            }
            copy_file_dereference(source, dest)?;
        }
        Action::GitAdd { paths } => git::git_add(repo_path, paths)?,
        Action::GitCommit { message } => git::git_commit(repo_path, message)?,
    }
    Ok(())
}

/// Return the inverse filesystem action, or `None` if not reversible.
///
/// Filesystem actions (CreateDir, Backup, CopyFile, CreateSymlink) are
/// reversible. RemoveFile / RemoveSymlink return None because the original
/// content is not tracked (the file was already removed from management;
/// to restore it, the user would need to re-add it or use `git checkout`).
/// Git actions (GitAdd, GitCommit) are handled separately in
/// `rollback_completed` via `git reset`.
pub(crate) fn action_rollback(action: &Action) -> Option<Action> {
    match action {
        Action::CreateDir { path } => Some(Action::RemoveFile { path: path.clone() }),
        Action::Backup { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::CopyFile { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::CreateSymlink {
            link, backup_path, ..
        } => {
            if let Some(backup) = backup_path {
                if backup.exists() {
                    Some(Action::RestoreBackup {
                        source: backup.clone(),
                        dest: link.clone(),
                    })
                } else {
                    Some(Action::RemoveSymlink { path: link.clone() })
                }
            } else {
                Some(Action::RemoveSymlink { path: link.clone() })
            }
        }
        Action::RemoveFile { path: _ } => None,
        Action::RemoveSymlink { path: _, .. } => None,
        Action::RestoreBackup { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::GitAdd { .. } => None,
        Action::GitCommit { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Plan execution
// ---------------------------------------------------------------------------

/// Execute all actions in the plan.
///
/// If `dry_run` is true, print each action but perform no mutations.
/// If any action fails, roll back all previously completed actions in
/// reverse order.
///
/// `state_path` is used to save a pending plan before execution for
/// crash recovery. The pending plan is cleared on success.
pub(crate) fn execute_plan(
    plan: &super::Plan,
    dry_run: bool,
    state_path: &Path,
) -> Result<(), DottyError> {
    if plan.is_empty() {
        return Ok(());
    }

    if dry_run {
        debug!("dry-run: {} actions", plan.actions.len());
        println!("[dry-run] Plan ({} actions):", plan.actions.len());
        for (i, action) in plan.actions.iter().enumerate() {
            println!("[dry-run]  {}. {}", i + 1, action);
        }
        println!("[dry-run] no changes made");
        return Ok(());
    }

    // Save pending plan for crash recovery
    crate::plan::save_pending_plan(plan, state_path)?;

    let mut completed: Vec<usize> = Vec::new();
    let check = crate::symbols::check();
    let use_progress_bar = plan.actions.len() > 20;
    let mut pb: Option<ProgressBar> = if use_progress_bar {
        Some(ProgressBar::new(plan.actions.len() as u64))
    } else {
        None
    };

    for (i, action) in plan.actions.iter().enumerate() {
        trace!("executing action {}: {}", i + 1, action);
        if use_progress_bar {
            if let Some(ref bar) = pb {
                bar.set_message(format!("{action}"));
            }
        } else {
            print!("  {}. {} ... ", i + 1, action);
        }
        match action.execute(&plan.repo_path) {
            Ok(()) => {
                if use_progress_bar {
                    if let Some(ref bar) = pb {
                        bar.inc(1);
                    }
                } else {
                    println!("{check}");
                }
                completed.push(i);
            }
            Err(e) => {
                warn!("action {} failed: {}", i + 1, e);
                if use_progress_bar && let Some(ref bar) = pb {
                    bar.finish();
                }
                println!("FAILED: {e}");
                rollback_completed(plan, &completed)?;
                return Err(e);
            }
        }
    }

    if use_progress_bar && let Some(bar) = pb.take() {
        bar.finish_and_clear();
    }

    // All actions succeeded — clear pending plan
    crate::plan::clear_pending_plan(state_path)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Rollback logic
// ---------------------------------------------------------------------------

/// A rollback operation that can be executed independently.
///
/// Filesystem rollbacks delegate to `Action::rollback()`. Git rollbacks
/// (`GitResetSoft`, `GitResetHead`) use dedicated git commands because their
/// inverse is not expressible as a simple `Action`.
#[derive(Debug)]
enum RollbackAction {
    /// Rollback a filesystem action by executing its inverse `Action`.
    Filesystem(Action),
    /// Undo the last commit via `git reset --soft HEAD~1`.
    GitResetSoft,
    /// Unstage files via `git reset HEAD <paths>`.
    GitResetHead { paths: Vec<PathBuf> },
}

impl RollbackAction {
    /// Execute this rollback operation.
    fn execute(&self, repo_path: &Path) -> Result<(), DottyError> {
        match self {
            RollbackAction::Filesystem(action) => action_execute(action, repo_path),
            RollbackAction::GitResetSoft => git::git_reset_soft_head(repo_path),
            RollbackAction::GitResetHead { paths } => {
                let path_strs: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
                git::git_reset(repo_path, &path_strs)
            }
        }
    }

    /// Format a human-readable description for logging.
    fn display(&self) -> String {
        match self {
            RollbackAction::Filesystem(action) => format!("{action}"),
            RollbackAction::GitResetSoft => "git reset --soft HEAD~1".to_string(),
            RollbackAction::GitResetHead { paths } => {
                let path_strs: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
                format!("git reset HEAD {}", path_strs.join(" "))
            }
        }
    }

    /// Convert an `Action` into the appropriate `RollbackAction`.
    ///
    /// Returns `None` if the action has no rollback (e.g. `RemoveFile`).
    fn from_action(action: &Action) -> Option<RollbackAction> {
        match action {
            Action::GitCommit { .. } => Some(RollbackAction::GitResetSoft),
            Action::GitAdd { paths } => {
                if paths.is_empty() {
                    None
                } else {
                    Some(RollbackAction::GitResetHead {
                        paths: paths.clone(),
                    })
                }
            }
            _ => action_rollback(action).map(RollbackAction::Filesystem),
        }
    }
}

/// Roll back completed actions in reverse order.
///
/// Each action is converted to a `RollbackAction` (filesystem or git) and
/// executed in reverse order. Git actions are batched per type so that
/// `git reset HEAD` is called once with all paths.
fn rollback_completed(plan: &super::Plan, completed_indices: &[usize]) -> Result<(), DottyError> {
    debug!("rolling back {} completed actions", completed_indices.len());
    let actions = &plan.actions;
    let repo_path = &plan.repo_path;

    let mut indices: Vec<usize> = completed_indices.to_vec();
    indices.sort_unstable();
    indices.reverse();

    // Collect all rollback actions, then execute in reverse order.
    // GitAdd rollbacks are batched: all paths are collected and reset in one call.
    let mut rollbacks: Vec<RollbackAction> = Vec::new();
    let mut git_add_paths: Vec<PathBuf> = Vec::new();

    for &idx in &indices {
        let action = &actions[idx];
        if let Some(rb) = RollbackAction::from_action(action) {
            match &rb {
                RollbackAction::GitResetHead { paths } => {
                    git_add_paths.extend(paths.clone());
                }
                _ => rollbacks.push(rb),
            }
        }
    }

    // Execute non-GitAdd rollbacks in order
    for rb in &rollbacks {
        println!("  rollback: {}", rb.display());
        rb.execute(repo_path)?;
    }

    // Batch GitAdd rollback (all paths in one git reset call)
    if !git_add_paths.is_empty() {
        let rb = RollbackAction::GitResetHead {
            paths: git_add_paths,
        };
        println!("  rollback: {}", rb.display());
        rb.execute(repo_path)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Copy a file, dereferencing symlinks (equivalent to `cp -L`).
pub(crate) fn copy_file_dereference(source: &Path, dest: &Path) -> Result<(), DottyError> {
    fs::copy(source, dest).map(|_| ())?;
    Ok(())
}

/// Verify that a backup file was created correctly.
///
/// Checks that the backup exists at the destination path and that its size
/// matches the source file. Returns an error if either check fails.
pub(crate) fn verify_backup_integrity(source: &Path, dest: &Path) -> Result<(), DottyError> {
    let dest_meta = fs::metadata(dest).map_err(|e| DottyError::BackupVerification {
        path: dest.to_path_buf(),
        detail: format!("backup file does not exist or is not readable: {}", e),
    })?;
    let source_meta = fs::metadata(source).map_err(|e| DottyError::BackupVerification {
        path: dest.to_path_buf(),
        detail: format!("cannot read source file metadata for comparison: {}", e),
    })?;

    let source_size = source_meta.len();
    let dest_size = dest_meta.len();

    if source_size != dest_size {
        return Err(DottyError::BackupVerification {
            path: dest.to_path_buf(),
            detail: format!(
                "size mismatch: source is {} bytes, backup is {} bytes",
                source_size, dest_size
            ),
        });
    }

    debug!("backup verified: {} ({} bytes)", dest.display(), dest_size);
    Ok(())
}

/// Convert an IO error into a more specific DottyError.
fn io_error_with_path(err: io::Error, path: &Path) -> DottyError {
    if err.kind() == io::ErrorKind::PermissionDenied {
        DottyError::PermissionDenied {
            path: path.to_path_buf(),
        }
    } else {
        DottyError::Io(err)
    }
}
