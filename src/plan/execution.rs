//! Plan execution and rollback logic.
//!
//! Contains [`execute_plan`], the [`RollbackAction`] enum, and helper functions
//! for executing individual actions, rolling back completed actions, and
//! copying/verifying files.

use std::fs;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::{debug, trace, warn};

use indicatif::ProgressBar;

use crate::error::DottyError;
use crate::git;
use crate::repo_state::RepoState;
use crate::symlink::{self, is_symlink};

use super::Action;

// ---------------------------------------------------------------------------
// Action execution and rollback (moved from plan.rs)
// ---------------------------------------------------------------------------

/// Execute the filesystem or git mutation described by this action.
///
/// `repo_state` provides the working directory for git operations and
/// caches the git identity validation result.
pub(crate) fn action_execute(
    action: &Action,
    repo_state: &mut RepoState,
) -> Result<(), DottyError> {
    match action {
        Action::CreateDir { path } => exec_create_dir(path)?,
        Action::Backup { source, dest } => exec_backup(source, dest)?,
        Action::BackupDir {
            source,
            dest,
            follow_symlinks,
        } => exec_backup_dir(source, dest, *follow_symlinks)?,
        Action::CopyFile { source, dest } => exec_copy_file(source, dest)?,
        Action::CreateSymlink { target, link, .. } => exec_create_symlink(target, link)?,
        Action::RemoveFile { path } => exec_remove_file(path)?,
        Action::RemoveDir { path } => exec_remove_dir(path)?,
        Action::RemoveSymlink { path } => exec_remove_symlink(path)?,
        Action::OrphanRemoved { path } => exec_orphan_removed(path)?,
        Action::RestoreBackup { source, dest } => exec_restore_backup(source, dest)?,
        Action::RestoreDir { source, dest } => exec_restore_dir(source, dest)?,
        Action::GitAdd { paths } => git::git_add(&repo_state.repo_path, paths)?,
        Action::GitCommit { message } => git::git_commit(repo_state, message)?,
        Action::Confirm { prompt, actions } => exec_confirm(prompt, actions, repo_state)?,
        Action::AbortGate { prompt } => exec_abort_gate(prompt)?,
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Extracted action execution functions
// ---------------------------------------------------------------------------

/// Create a directory (and all parents).
fn exec_create_dir(path: &Path) -> Result<(), DottyError> {
    fs::create_dir_all(path).map_err(|e| io_error_with_path(e, path))
}

/// Backup a file to the backup directory with integrity verification.
///
/// Computes the source hash *before* `copy_file` to avoid false-positive
/// `BackupHashMismatch` when the source file is modified concurrently
/// (e.g., editor autosave) between copy and verification.
fn exec_backup(source: &Path, dest: &Path) -> Result<(), DottyError> {
    let parent = dest.parent().ok_or_else(|| DottyError::PathResolution {
        path: dest.to_path_buf(),
        reason: format!("cannot determine parent of backup path: {}", dest.display()),
    })?;
    fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
    let source_hash = compute_file_hash(source).ok();
    copy_file(source, dest)?;
    verify_backup_integrity(source, dest, source_hash.as_deref())
}

/// Recursively backup a directory to the backup location.
fn exec_backup_dir(source: &Path, dest: &Path, follow_symlinks: bool) -> Result<(), DottyError> {
    let parent = dest.parent().ok_or_else(|| DottyError::PathResolution {
        path: dest.to_path_buf(),
        reason: format!(
            "cannot determine parent of backup dir path: {}",
            dest.display()
        ),
    })?;
    fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
    copy_dir(source, dest, follow_symlinks)
}

/// Copy a file, creating parent directories and replacing existing symlinks first.
///
/// Removes an existing symlink at `dest` before copying so `fs::copy` creates
/// a regular file (fs::copy follows symlinks, writing to the target instead of
/// replacing the symlink itself).
fn exec_copy_file(source: &Path, dest: &Path) -> Result<(), DottyError> {
    let parent = dest.parent();
    if let Some(p) = parent {
        fs::create_dir_all(p).map_err(|e| io_error_with_path(e, p))?;
    }
    if is_symlink(dest) {
        fs::remove_file(dest).map_err(|e| io_error_with_path(e, dest))?;
    }
    copy_file(source, dest)
}

/// Create a symlink, with data-loss protection via a temp path.
///
/// Creates the symlink at a temp path first to avoid data loss. If creation
/// fails, the original file at `link` is untouched. For non-symlink directories,
/// `rename(2)` cannot replace atomically on Unix (ENOTEMPTY), so the directory
/// is removed first. Then the symlink is atomically renamed into place.
fn exec_create_symlink(target: &Path, link: &Path) -> Result<(), DottyError> {
    let parent = link.parent();
    if let Some(p) = parent {
        fs::create_dir_all(p).map_err(|e| io_error_with_path(e, p))?;
    }
    if symlink::would_be_circular(target, link) {
        return Err(DottyError::CircularSymlink {
            path: link.to_path_buf(),
        });
    }
    let temp_name = format!(
        ".{}_dotty_tmp",
        link.file_name().unwrap_or_default().to_string_lossy()
    );
    let temp_path = link.with_file_name(temp_name);
    if let Err(e) = crate::symlink::create_symlink(target, &temp_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(io_error_with_path(e, link));
    }
    if fs::symlink_metadata(link).is_ok() && link.is_dir() && !crate::symlink::is_symlink(link) {
        fs::remove_dir_all(link).map_err(|e| io_error_with_path(e, link))?;
    }
    fs::rename(&temp_path, link).map_err(|e| io_error_with_path(e, link))?;
    Ok(())
}

/// Remove a file, if it exists.
fn exec_remove_file(path: &Path) -> Result<(), DottyError> {
    if path.exists() {
        fs::remove_file(path).map_err(|e| io_error_with_path(e, path))?;
    }
    Ok(())
}

/// Remove a directory (recursively), if it exists.
fn exec_remove_dir(path: &Path) -> Result<(), DottyError> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|e| io_error_with_path(e, path))?;
    }
    Ok(())
}

/// Remove a symlink, warning if it points to a directory (content preserved).
fn exec_remove_symlink(path: &Path) -> Result<(), DottyError> {
    if is_symlink(path) {
        if let Ok(target) = fs::read_link(path)
            && target.is_dir()
        {
            warn!(
                "Removing symlink to directory: {} → {} (directory content preserved)",
                path.display(),
                target.display()
            );
        }
        remove_symlink_file(path).map_err(|e| io_error_with_path(e, path))?;
    }
    Ok(())
}

/// Remove an orphan target (file/dir/symlink), detecting the file type at
/// execution time.
///
/// File type is determined at execution time so the action remains correct even
/// if the orphan changed between plan-build and execution. Uses
/// `symlink_metadata` to detect symlinks without following them.
fn exec_orphan_removed(path: &Path) -> Result<(), DottyError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                remove_symlink_file(path).map_err(|e| io_error_with_path(e, path))?;
            } else if file_type.is_dir() {
                fs::remove_dir_all(path).map_err(|e| io_error_with_path(e, path))?;
            } else {
                fs::remove_file(path).map_err(|e| io_error_with_path(e, path))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(io_error_with_path(e, path));
        }
    }
    Ok(())
}

/// Restore a backup file, with TOCTOU race mitigation.
///
/// TOCTOU race condition mitigation: the backup could have been deleted by
/// a concurrent `dotty clean` between plan construction and rollback execution.
/// Existence is checked at rollback time for graceful degradation.
fn exec_restore_backup(source: &Path, dest: &Path) -> Result<(), DottyError> {
    if is_symlink(dest) {
        remove_symlink_file(dest).map_err(|e| io_error_with_path(e, dest))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
    }
    if !source.exists() {
        warn!(
            "Backup file deleted during rollback (race with `dotty clean`), \
            preserving original intent by removing symlink: {}",
            dest.display()
        );
        return Ok(());
    }
    copy_file(source, dest)
}

/// Restore a backup directory, with TOCTOU race mitigation.
///
/// TOCTOU race condition mitigation: the backup could have been deleted by
/// a concurrent `dotty clean` between plan construction and rollback execution.
/// Existence is checked at rollback time for graceful degradation. Always
/// follows symlinks since the backup was created with the actual content.
fn exec_restore_dir(source: &Path, dest: &Path) -> Result<(), DottyError> {
    if dest.exists() {
        if dest.is_dir() && !is_symlink(dest) {
            fs::remove_dir_all(dest).map_err(|e| io_error_with_path(e, dest))?;
        } else if is_symlink(dest) {
            remove_symlink_file(dest).map_err(|e| io_error_with_path(e, dest))?;
        }
    }
    if !source.exists() {
        warn!(
            "Backup directory deleted during rollback (race with `dotty clean`), \
            skipping restoration: {}",
            source.display()
        );
        return Ok(());
    }
    copy_dir(source, dest, true)
}

/// Execute actions guarded by a confirmation prompt.
///
/// When `prompt` is `None`, child actions execute unconditionally. When
/// `prompt` is `Some`, execution is interactive: if the user confirms, each
/// action is executed with rollback on partial failure; if declined, all
/// guarded actions are skipped. In non-interactive contexts the guarded
/// actions are silently skipped.
fn exec_confirm(
    prompt: &Option<String>,
    actions: &[Action],
    repo_state: &mut RepoState,
) -> Result<(), DottyError> {
    match prompt {
        None => {
            for action in actions {
                action_execute(action, repo_state)?;
            }
        }
        Some(p) => {
            if !crate::prompt::is_interactive() {
                warn!(
                    "non-interactive context: skipping {} action(s) guarded by confirm",
                    actions.len()
                );
                return Ok(());
            }
            let confirmed = crate::prompt::prompt_confirm(p, true)?;
            if !confirmed {
                return Ok(());
            }
            let mut completed: Vec<usize> = Vec::new();
            for (i, action) in actions.iter().enumerate() {
                match action_execute(action, repo_state) {
                    Ok(()) => {
                        completed.push(i);
                    }
                    Err(e) => {
                        for &idx in completed.iter().rev() {
                            if let Some(action) = actions.get(idx)
                                && let Some(rollback) = action_rollback(action)
                            {
                                let _ = action_execute(&rollback, repo_state);
                            }
                        }
                        return Err(e);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Gate that aborts the entire plan execution if the user declines.
///
/// Unlike `Confirm` (which skips guarded actions on decline), this returns
/// an error that propagates up and triggers rollback of all previously
/// completed actions. In non-interactive contexts, the gate is skipped
/// with a warning.
fn exec_abort_gate(prompt: &str) -> Result<(), DottyError> {
    if !crate::prompt::is_interactive() {
        warn!("non-interactive context: abort gate skipped — \"{prompt}\"");
        return Ok(());
    }
    let confirmed = crate::prompt::prompt_confirm(prompt, true)?;
    if !confirmed {
        return Err(DottyError::Cancelled);
    }
    Ok(())
}

/// Return the inverse filesystem action, or `None` if not reversible.
///
/// Filesystem actions (CreateDir, Backup, CopyFile, CreateSymlink) are
/// reversible. RemoveFile / RemoveDir / RemoveSymlink return None because the
/// original content is not tracked (the file was already removed from
/// management; to restore it, the user would need to re-add it or use
/// `git checkout`).
/// Git actions (GitAdd, GitCommit) are handled separately in
/// `rollback_completed` via `git reset`.
pub(crate) fn action_rollback(action: &Action) -> Option<Action> {
    match action {
        Action::CreateDir { path } => Some(Action::RemoveDir { path: path.clone() }),
        Action::Backup { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::BackupDir { dest, .. } => Some(Action::RemoveDir { path: dest.clone() }),
        Action::CopyFile { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::CreateSymlink {
            link,
            backup_path,
            target: _,
        } => {
            // Check backup existence at rollback time to avoid storing stale
            // metadata from plan-build time. The backup may have been deleted
            // between execution and rollback (e.g., by a concurrent `dotty clean`).
            // If the backup doesn't exist, fall back to simple symlink removal.
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
        Action::RemoveDir { path: _ } => None,
        Action::RemoveSymlink { path: _, .. } => None,
        Action::OrphanRemoved { path: _ } => None,
        Action::RestoreBackup { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::RestoreDir { dest, .. } => Some(Action::RemoveDir { path: dest.clone() }),
        Action::GitAdd { .. } => None,
        Action::GitCommit { .. } => None,
        Action::Confirm { actions, .. } => {
            let rollback_actions: Vec<Action> =
                actions.iter().rev().filter_map(action_rollback).collect();
            if rollback_actions.is_empty() {
                None
            } else {
                Some(Action::Confirm {
                    prompt: None,
                    actions: rollback_actions,
                })
            }
        }
        Action::AbortGate { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Plan execution
// ---------------------------------------------------------------------------

/// Execution mode for [`execute_plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecuteMode {
    /// Save pending plan, clear on success.
    Normal,
    /// Dry-run: no mutations, no pending plan.
    DryRun,
    /// No pending plan (avoids nested pending plans during rollback).
    Rollback,
}

impl ExecuteMode {
    /// Whether this mode skips all mutations.
    fn is_dry_run(&self) -> bool {
        matches!(self, ExecuteMode::DryRun)
    }

    /// Whether this mode saves/clears a pending plan for crash recovery.
    fn save_pending(&self) -> bool {
        matches!(self, ExecuteMode::Normal)
    }
}

/// Execute all actions in the plan, rolling back completed actions in reverse
/// order on failure.
pub(crate) fn execute_plan(
    plan: &super::Plan,
    mode: ExecuteMode,
    repo_state: &mut RepoState,
) -> Result<(), DottyError> {
    if plan.is_empty() {
        return Ok(());
    }

    if mode.is_dry_run() {
        debug!("dry-run: {} actions", plan.actions.len());
        for action in &plan.actions {
            println!("[dry-run] {action}");
        }
        return Ok(());
    }

    let check = crate::symbols::check();

    // Save pending plan for crash recovery (skipped for rollback/dry-run)
    if mode.save_pending() {
        crate::plan::save_pending_plan(plan, &repo_state.state_path)?;
    }

    /// Minimum number of actions to use a progress bar.
    const PLAN_PROGRESS_BAR_THRESHOLD: usize = 20;

    let mut completed: Vec<usize> = Vec::new();
    let total = plan.actions.len();
    let use_progress_bar = total > PLAN_PROGRESS_BAR_THRESHOLD;
    let mut pb: Option<ProgressBar> = if use_progress_bar {
        Some(ProgressBar::new(total as u64))
    } else {
        None
    };

    for (i, action) in plan.actions.iter().enumerate() {
        trace!("executing action {}: {}", i + 1, action);
        let is_noop = matches!(
            action,
            Action::RemoveSymlink { path } if !is_symlink(path)
        );
        match action_execute(action, repo_state) {
            Ok(()) => {
                if use_progress_bar {
                    if let Some(ref bar) = pb {
                        if !is_noop {
                            bar.set_message(format!("{check} {action}"));
                        }
                        bar.inc(1);
                    }
                } else if !is_noop {
                    println!("{check} {action}");
                }
                completed.push(i);
            }
            Err(e) => {
                warn!("action {} failed: {}", i + 1, e);
                if use_progress_bar && let Some(ref bar) = pb {
                    bar.finish();
                }
                println!("FAILED: {e}");
                rollback_completed(plan, &completed, repo_state)?;
                return Err(e);
            }
        }
    }

    if use_progress_bar && let Some(bar) = pb.take() {
        bar.finish_and_clear();
    }

    // All actions succeeded — clear pending plan (skipped for rollback/dry-run)
    if mode.save_pending() {
        crate::plan::clear_pending_plan(&repo_state.state_path)?;
    }

    // Note: the trailing `done` line is printed by the summary (after
    // override annotations, before counts) — not by the executor. The
    // executor only prints per-action lines.

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
    /// Undo `depth` commits via `git reset --soft HEAD~{depth}`.
    GitResetSoft { depth: usize },
    /// Unstage files via `git reset HEAD <paths>`.
    GitResetHead { paths: Vec<PathBuf> },
}

impl RollbackAction {
    /// Execute this rollback operation.
    fn execute(&self, repo_state: &mut RepoState) -> Result<(), DottyError> {
        match self {
            RollbackAction::Filesystem(action) => action_execute(action, repo_state),
            RollbackAction::GitResetSoft { depth } => {
                git::git_reset_soft_head(&repo_state.repo_path, *depth)
            }
            RollbackAction::GitResetHead { paths } => {
                let path_strs: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
                git::git_reset(&repo_state.repo_path, &path_strs)
            }
        }
    }

    /// Format a human-readable description for logging.
    fn display(&self) -> String {
        match self {
            RollbackAction::Filesystem(action) => format!("{action}"),
            RollbackAction::GitResetSoft { depth } => {
                format!("git reset --soft HEAD~{depth}")
            }
            RollbackAction::GitResetHead { paths } => {
                let path_strs: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
                format!("git reset HEAD {}", path_strs.join(" "))
            }
        }
    }

    /// Convert an `Action` into the appropriate `RollbackAction`.
    ///
    /// Returns `None` if the action has no rollback (e.g. `RemoveFile`, `RemoveDir`).
    fn from_action(action: &Action) -> Option<RollbackAction> {
        match action {
            Action::GitCommit { .. } => Some(RollbackAction::GitResetSoft { depth: 1 }),
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
/// `git reset HEAD` is called once with all paths, and `git reset --soft`
/// undoes all committed changes in a single call (`HEAD~N`).
fn rollback_completed(
    plan: &super::Plan,
    completed_indices: &[usize],
    repo_state: &mut RepoState,
) -> Result<(), DottyError> {
    debug!("rolling back {} completed actions", completed_indices.len());
    let actions = &plan.actions;

    let mut indices: Vec<usize> = completed_indices.to_vec();
    indices.sort_unstable();
    indices.reverse();

    // Collect all rollback actions, then execute in reverse order.
    // GitAdd rollbacks are batched: all paths are collected and reset in one call.
    // GitCommit rollbacks are batched: all commits are undone in one reset.
    let mut rollbacks: Vec<RollbackAction> = Vec::new();
    let mut git_add_paths: Vec<PathBuf> = Vec::new();
    let mut git_commit_count: usize = 0;

    for &idx in &indices {
        let Some(action) = actions.get(idx) else {
            continue;
        };
        if let Some(rb) = RollbackAction::from_action(action) {
            match &rb {
                RollbackAction::GitResetHead { paths } => {
                    git_add_paths.extend(paths.clone());
                }
                RollbackAction::GitResetSoft { .. } => {
                    git_commit_count += 1;
                }
                _ => rollbacks.push(rb),
            }
        }
    }

    // Execute non-git rollbacks in order
    for rb in &rollbacks {
        println!("  rollback: {}", rb.display());
        rb.execute(repo_state)?;
    }

    // Batch GitCommit rollback (all commits undone in one reset call)
    if git_commit_count > 0 {
        let rb = RollbackAction::GitResetSoft {
            depth: git_commit_count,
        };
        println!("  rollback: {}", rb.display());
        rb.execute(repo_state)?;
    }

    // Batch GitAdd rollback (all paths in one git reset call)
    if !git_add_paths.is_empty() {
        let rb = RollbackAction::GitResetHead {
            paths: git_add_paths,
        };
        println!("  rollback: {}", rb.display());
        rb.execute(repo_state)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Copies a file, following symlinks (equivalent to `cp -L`).
///
/// `std::fs::copy` already follows symlinks on all supported platforms,
/// so no explicit dereferencing is needed. This wrapper exists to make
/// the intent clear at call sites.
pub(crate) fn copy_file(source: &Path, dest: &Path) -> Result<(), DottyError> {
    let bytes = fs::copy(source, dest)?;
    debug!(
        "copied {} bytes: {} → {}",
        bytes,
        source.display(),
        dest.display()
    );
    Ok(())
}

/// Recursively copy a directory from `source` to `dest`.
///
/// Creates the destination directory and copies all files and subdirectories
/// recursively. When `follow_symlinks` is false (default), symlinked files
/// are skipped to prevent exposing sensitive data outside the intended home
/// directory. Symlinked directories are always skipped regardless of this flag.
pub(crate) fn copy_dir(
    source: &Path,
    dest: &Path,
    follow_symlinks: bool,
) -> Result<(), DottyError> {
    // Create the destination directory
    fs::create_dir_all(dest).map_err(|e| io_error_with_path(e, dest))?;

    // Walk the source directory and copy each file
    let mut files = Vec::new();
    crate::fs_utils::walk_dir(source, &mut files)?;

    for file_path in &files {
        // Skip symlinked files when follow_symlinks is false.
        // This prevents copying sensitive data from outside the intended
        // home directory into the backup (e.g., /etc/shadow, SSH keys).
        if !follow_symlinks
            && file_path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            warn!(
                "Skipping symlinked file during backup: {}",
                file_path.display()
            );
            continue;
        }

        // Compute the relative path from source
        let relative = file_path
            .strip_prefix(source)
            .map_err(|e| DottyError::PathResolution {
                path: file_path.clone(),
                reason: format!("cannot strip source prefix: {}", e),
            })?;

        let dest_file = dest.join(relative);
        // Ensure parent directory exists
        if let Some(parent) = dest_file.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
        }
        copy_file(file_path, &dest_file)?;
    }

    Ok(())
}

/// Compute SHA-256 hash of a file.
///
/// Returns the hash as a lowercase hexadecimal string (64 characters).
fn compute_file_hash(path: &Path) -> Result<String, DottyError> {
    let file = fs::File::open(path).map_err(DottyError::Io)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(DottyError::Io)?;
        if n == 0 {
            break;
        }
        if let Some(slice) = buf.get(..n) {
            hasher.update(slice);
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect())
}

/// Verify that a backup file was created correctly.
///
/// Uses a two-tier verification strategy for performance:
/// - Files ≤ 1KB: size check only (fast, catches most corruption)
/// - Files > 1KB: size check + SHA-256 hash verification (strong integrity)
///
/// If `expected_source_hash` is provided, it is used instead of re-hashing the
/// live source file. This prevents false-positive `BackupHashMismatch` when the
/// source file is modified concurrently (e.g., editor autosave) between the
/// copy and verification step.
pub(crate) fn verify_backup_integrity(
    source: &Path,
    dest: &Path,
    expected_source_hash: Option<&str>,
) -> Result<(), DottyError> {
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

    const HASH_VERIFICATION_THRESHOLD: u64 = 1024;
    if source_size > HASH_VERIFICATION_THRESHOLD {
        let source_hash = match expected_source_hash {
            Some(h) => h.to_string(),
            None => compute_file_hash(source)?,
        };
        let dest_hash = compute_file_hash(dest)?;

        if source_hash != dest_hash {
            return Err(DottyError::BackupHashMismatch {
                path: dest.to_path_buf(),
                expected_hash: source_hash,
                actual_hash: dest_hash,
            });
        }
    }

    debug!("backup verified: {} ({} bytes)", dest.display(), dest_size);
    Ok(())
}

/// Remove a symlink file, handling Windows directory symlinks correctly.
///
/// On Windows, directory symlinks (junctions/reparse points) must be removed
/// with `remove_dir` rather than `remove_file`. On Unix, both file and
/// directory symlinks are removed with `remove_file`.
fn remove_symlink_file(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    if path.is_dir() {
        return fs::remove_dir(path);
    }
    fs::remove_file(path)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use crate::plan::Plan;
    use crate::repo_state::RepoState;
    use crate::symlink::create_symlink;

    /// Test SHA-256 hash computation produces expected result.
    #[test]
    fn test_compute_file_hash_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let hash = compute_file_hash(&file).unwrap();

        // SHA-256 of "hello world" is a known value
        assert_eq!(hash.len(), 64, "SHA-256 hash should be 64 hex characters");
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    /// Test SHA-256 hash is deterministic (same input = same output).
    #[test]
    fn test_compute_file_hash_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").unwrap();

        let hash1 = compute_file_hash(&file).unwrap();
        let hash2 = compute_file_hash(&file).unwrap();

        assert_eq!(hash1, hash2, "Same file should produce same hash");
    }

    /// Test SHA-256 hash differs for different content.
    #[test]
    fn test_compute_file_hash_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("file1.txt");
        let file2 = dir.path().join("file2.txt");
        fs::write(&file1, "content A").unwrap();
        fs::write(&file2, "content B").unwrap();

        let hash1 = compute_file_hash(&file1).unwrap();
        let hash2 = compute_file_hash(&file2).unwrap();

        assert_ne!(hash1, hash2, "Different files should have different hashes");
    }

    /// Test hash computation fails for non-existent file.
    #[test]
    fn test_compute_file_hash_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nonexistent.txt");

        let result = compute_file_hash(&file);
        assert!(result.is_err());
    }

    /// Test verify_backup_integrity passes for identical files ≤ 1KB.
    #[test]
    fn test_verify_backup_integrity_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        fs::write(&source, "small content").unwrap();
        fs::copy(&source, &dest).unwrap();

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(
            result.is_ok(),
            "Small identical files should pass verification"
        );
    }

    /// Test verify_backup_integrity passes for identical files > 1KB.
    #[test]
    fn test_verify_backup_integrity_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        // Create a file > 1KB (1025 bytes)
        let content = "x".repeat(1025);
        fs::write(&source, &content).unwrap();
        fs::copy(&source, &dest).unwrap();

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(
            result.is_ok(),
            "Large identical files should pass verification"
        );
    }

    /// Test verify_backup_integrity fails for size mismatch.
    #[test]
    fn test_verify_backup_integrity_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        fs::write(&source, "original content").unwrap();
        fs::write(&dest, "short").unwrap();

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(result.is_err());
        match result.unwrap_err() {
            DottyError::BackupVerification { path, detail } => {
                assert_eq!(path, dest);
                assert!(detail.contains("size mismatch"));
            }
            _ => panic!("Expected BackupVerification error"),
        }
    }

    /// Test verify_backup_integrity fails for hash mismatch (> 1KB).
    #[test]
    fn test_verify_backup_integrity_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        // Create files with same size but different content (> 1KB)
        let source_content = "x".repeat(2000);
        let dest_content = "y".repeat(2000);
        fs::write(&source, &source_content).unwrap();
        fs::write(&dest, &dest_content).unwrap();

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(result.is_err());
        match result.unwrap_err() {
            DottyError::BackupHashMismatch {
                path,
                expected_hash,
                actual_hash,
            } => {
                assert_eq!(path, dest);
                assert_ne!(expected_hash, actual_hash);
                assert_eq!(expected_hash.len(), 64);
                assert_eq!(actual_hash.len(), 64);
            }
            _ => panic!("Expected BackupHashMismatch error"),
        }
    }

    /// Test verify_backup_integrity fails for non-existent backup.
    #[test]
    fn test_verify_backup_integrity_nonexistent_backup() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("nonexistent.txt");
        fs::write(&source, "content").unwrap();

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(result.is_err());
    }

    /// Test verify_backup_integrity at exactly 1KB boundary.
    #[test]
    fn test_verify_backup_integrity_exactly_1kb() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        // Exactly 1024 bytes = 1KB (should use size-only check)
        let content = "x".repeat(1024);
        fs::write(&source, &content).unwrap();
        fs::copy(&source, &dest).unwrap();

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(
            result.is_ok(),
            "Exactly 1KB file should pass with size check"
        );
    }

    /// Test verify_backup_integrity at 1KB + 1 byte boundary.
    #[test]
    fn test_verify_backup_integrity_1kb_plus_1() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        // 1025 bytes = 1KB + 1 byte (should trigger hash verification)
        let content = "x".repeat(1025);
        fs::write(&source, &content).unwrap();
        fs::copy(&source, &dest).unwrap();

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(
            result.is_ok(),
            "1KB+1 file should pass with hash verification"
        );
    }

    /// Test that a pre-computed source hash prevents false-positive
    /// BackupHashMismatch when the source file is modified concurrently
    /// (simulated by modifying the source after copy).
    #[test]
    fn test_verify_backup_integrity_precomputed_hash_avoids_race() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        let original = "original content ".repeat(200);
        let modified = "modified content ".repeat(200);

        fs::write(&source, &original).unwrap();

        let precomputed_hash = compute_file_hash(&source).unwrap();

        fs::copy(&source, &dest).unwrap();

        fs::write(&source, &modified).unwrap();

        let result = verify_backup_integrity(&source, &dest, Some(&precomputed_hash));
        assert!(
            result.is_ok(),
            "pre-computed hash should pass even though source was modified"
        );

        let result = verify_backup_integrity(&source, &dest, None);
        assert!(
            result.is_err(),
            "without pre-computed hash, verification should fail due to modified source"
        );
    }

    /// Test that copy_dir skips symlinked files by default (follow_symlinks=false).
    ///
    /// Creates a directory with a real file and a symlink to an external file.
    /// Verifies that only the real file is copied into the backup.
    #[test]
    fn test_copy_dir_skips_symlinks_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();

        // Create a real file inside the source directory
        fs::write(source.join("real.txt"), "real content").unwrap();

        // Create a symlink pointing to a file outside the source directory
        let external_file = dir.path().join("external.txt");
        fs::write(&external_file, "external content").unwrap();
        create_symlink(&external_file, &source.join("link.txt")).unwrap();

        // Copy with follow_symlinks=false (default)
        copy_dir(&source, &dest, false).unwrap();

        // Real file should be copied
        assert!(dest.join("real.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("real.txt")).unwrap(),
            "real content"
        );

        // Symlink should NOT be copied
        assert!(!dest.join("link.txt").exists());
    }

    /// Test that copy_dir follows symlinks when follow_symlinks=true.
    ///
    /// Creates a directory with a real file and a symlink. Verifies that
    /// both are copied (symlink dereferenced to its target content).
    #[test]
    fn test_copy_dir_follows_symlinks_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();

        // Create a real file inside the source directory
        fs::write(source.join("real.txt"), "real content").unwrap();

        // Create a symlink pointing to a file outside the source directory
        let external_file = dir.path().join("external.txt");
        fs::write(&external_file, "external content").unwrap();
        create_symlink(&external_file, &source.join("link.txt")).unwrap();

        // Copy with follow_symlinks=true
        copy_dir(&source, &dest, true).unwrap();

        // Real file should be copied
        assert!(dest.join("real.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("real.txt")).unwrap(),
            "real content"
        );

        // Symlink should be dereferenced and its content copied
        assert!(dest.join("link.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("link.txt")).unwrap(),
            "external content"
        );
        // The copied file should NOT be a symlink itself
        assert!(
            !dest
                .join("link.txt")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// Test that copy_dir skips symlinked directories by default.
    ///
    /// Creates a directory with a real subdirectory and a symlink to an
    /// external directory. Verifies that the symlinked directory is not
    /// traversed.
    #[test]
    fn test_copy_dir_skips_symlinked_directories() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();

        // Create a real subdirectory with a file
        let real_sub = source.join("real_sub");
        fs::create_dir_all(&real_sub).unwrap();
        fs::write(real_sub.join("inner.txt"), "inner content").unwrap();

        // Create a symlink to a directory outside the source
        let external_dir = dir.path().join("external_dir");
        fs::create_dir_all(&external_dir).unwrap();
        fs::write(external_dir.join("sensitive.txt"), "secret").unwrap();
        create_symlink(&external_dir, &source.join("link_dir")).unwrap();

        // Copy with follow_symlinks=false
        copy_dir(&source, &dest, false).unwrap();

        // Real subdirectory should be copied
        assert!(dest.join("real_sub").is_dir());
        assert!(dest.join("real_sub").join("inner.txt").exists());

        // Symlinked directory should NOT be traversed
        assert!(!dest.join("link_dir").exists());
    }

    /// Test that copy_dir with follow_symlinks=true still skips symlinked
    /// directories (to prevent infinite loops and unintended traversal).
    #[test]
    fn test_copy_dir_skips_symlinked_directories_even_with_follow() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();

        // Create a symlink to a directory outside the source
        let external_dir = dir.path().join("external_dir");
        fs::create_dir_all(&external_dir).unwrap();
        fs::write(external_dir.join("sensitive.txt"), "secret").unwrap();
        create_symlink(&external_dir, &source.join("link_dir")).unwrap();

        // Copy with follow_symlinks=true
        copy_dir(&source, &dest, true).unwrap();

        // Symlinked directory should still NOT be traversed
        assert!(!dest.join("link_dir").exists());
    }

    /// Test that Action::BackupDir with follow_symlinks=false skips symlinks
    /// during execution.
    #[test]
    fn test_backup_dir_action_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source_dir");
        let backup = dir.path().join("backups/2024-01-01T00-00-00");
        fs::create_dir_all(&source).unwrap();

        // Create a real file
        fs::write(source.join("real.txt"), "real").unwrap();

        // Create a symlink to an external file
        let external = dir.path().join("external.txt");
        fs::write(&external, "external").unwrap();
        create_symlink(&external, &source.join("link.txt")).unwrap();

        let action = Action::BackupDir {
            source: source.clone(),
            dest: backup.clone(),
            follow_symlinks: false,
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(
                std::path::PathBuf::from("."),
                std::path::PathBuf::from("."),
            ),
        )
        .unwrap();

        // Real file should be backed up
        assert!(backup.join("real.txt").exists());
        assert_eq!(fs::read_to_string(backup.join("real.txt")).unwrap(), "real");

        // Symlink should NOT be backed up
        assert!(!backup.join("link.txt").exists());
    }

    /// Test that Action::BackupDir with follow_symlinks=true copies symlink content.
    #[test]
    fn test_backup_dir_action_follows_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source_dir");
        let backup = dir.path().join("backups/2024-01-01T00-00-00");
        fs::create_dir_all(&source).unwrap();

        // Create a real file
        fs::write(source.join("real.txt"), "real").unwrap();

        // Create a symlink to an external file
        let external = dir.path().join("external.txt");
        fs::write(&external, "external content").unwrap();
        create_symlink(&external, &source.join("link.txt")).unwrap();

        let action = Action::BackupDir {
            source: source.clone(),
            dest: backup.clone(),
            follow_symlinks: true,
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(
                std::path::PathBuf::from("."),
                std::path::PathBuf::from("."),
            ),
        )
        .unwrap();

        // Real file should be backed up
        assert!(backup.join("real.txt").exists());

        // Symlink should be dereferenced and content copied
        assert!(backup.join("link.txt").exists());
        assert_eq!(
            fs::read_to_string(backup.join("link.txt")).unwrap(),
            "external content"
        );
        assert!(
            !backup
                .join("link.txt")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// Test that original file survives when symlink creation fails.
    ///
    /// Creates a regular file at the link location, then makes the
    /// parent directory read-only so that create_symlink (at the temp
    /// path) fails. Asserts the original file is preserved intact.
    #[cfg(unix)]
    #[test]
    fn test_create_symlink_preserves_original_on_failure() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link_file");

        fs::write(&target, "target content").unwrap();
        fs::write(&link, "original content").unwrap();

        // Make parent read-only so create_symlink fails (cannot
        // create new files in a dir without write permission).
        fs::set_permissions(dir.path(), Permissions::from_mode(0o555)).unwrap();

        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
            backup_path: None,
        };
        let result = action_execute(
            &action,
            &mut RepoState::new_for_git(
                std::path::PathBuf::from("."),
                std::path::PathBuf::from("."),
            ),
        );

        assert!(
            result.is_err(),
            "CreateSymlink should fail on read-only dir"
        );
        assert!(
            link.exists(),
            "Original file should survive symlink creation failure"
        );
        assert_eq!(
            fs::read_to_string(&link).unwrap(),
            "original content",
            "Original file content should be unchanged"
        );
    }

    // -------------------------------------------------------------------
    // RestoreBackup tests
    // -------------------------------------------------------------------

    #[test]
    fn test_restore_backup_success() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup.txt");
        let dest = dir.path().join("restored.txt");
        fs::write(&source, "restored content").unwrap();

        let action = Action::RestoreBackup {
            source: source.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        )
        .unwrap();

        assert!(dest.exists());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "restored content");
    }

    #[test]
    fn test_restore_backup_missing_backup() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backups/backup.txt");
        let dest = dir.path().join("config/file.txt");

        let action = Action::RestoreBackup {
            source: source.clone(),
            dest: dest.clone(),
        };
        let result = action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        );

        assert!(
            result.is_ok(),
            "Missing backup should be handled gracefully"
        );
    }

    #[test]
    fn test_restore_backup_overwrites_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup.txt");
        let dest = dir.path().join("link.txt");
        let old_target = dir.path().join("old_target.txt");

        fs::write(&source, "restored content").unwrap();
        fs::write(&old_target, "old content").unwrap();
        create_symlink(&old_target, &dest).unwrap();

        let action = Action::RestoreBackup {
            source: source.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        )
        .unwrap();

        assert!(dest.exists());
        assert!(!is_symlink(&dest));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "restored content");
    }

    #[test]
    fn test_restore_backup_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup.txt");
        let dest = dir.path().join("nested/deep/restored.txt");

        fs::write(&source, "content").unwrap();

        let action = Action::RestoreBackup {
            source: source.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        )
        .unwrap();

        assert!(dest.exists());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "content");
    }

    #[cfg(unix)]
    #[test]
    fn test_restore_backup_permission_denied() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup.txt");
        let dest = dir.path().join("readonly/child/restored.txt");

        fs::write(&source, "content").unwrap();
        fs::create_dir_all(dir.path().join("readonly")).unwrap();
        fs::set_permissions(dir.path().join("readonly"), Permissions::from_mode(0o555)).unwrap();

        let action = Action::RestoreBackup {
            source: source.clone(),
            dest: dest.clone(),
        };
        let result = action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        );

        assert!(result.is_err());
    }

    // -------------------------------------------------------------------
    // RestoreDir tests
    // -------------------------------------------------------------------

    #[test]
    fn test_restore_dir_success() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup_dir");
        let dest = dir.path().join("restored_dir");

        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file1.txt"), "content1").unwrap();
        fs::write(source.join("file2.txt"), "content2").unwrap();

        let action = Action::RestoreDir {
            source: source.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        )
        .unwrap();

        assert!(dest.is_dir());
        assert!(dest.join("file1.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("file1.txt")).unwrap(),
            "content1"
        );
        assert!(dest.join("file2.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("file2.txt")).unwrap(),
            "content2"
        );
    }

    #[test]
    fn test_restore_dir_missing_backup() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backups/config");
        let dest = dir.path().join("config");

        let action = Action::RestoreDir {
            source: source.clone(),
            dest: dest.clone(),
        };
        let result = action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        );

        assert!(
            result.is_ok(),
            "Missing backup dir should be handled gracefully"
        );
    }

    #[test]
    fn test_restore_dir_overwrites_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup_dir");
        let dest = dir.path().join("existing_dir");

        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("new.txt"), "new content").unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("old.txt"), "old content").unwrap();

        let action = Action::RestoreDir {
            source: source.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        )
        .unwrap();

        assert!(!dest.join("old.txt").exists());
        assert!(dest.join("new.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("new.txt")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn test_restore_dir_overwrites_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup_dir");
        let dest = dir.path().join("link_dir");
        let sym_target = dir.path().join("actual_dir");

        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.txt"), "restored").unwrap();
        fs::create_dir_all(&sym_target).unwrap();
        create_symlink(&sym_target, &dest).unwrap();

        let action = Action::RestoreDir {
            source: source.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        )
        .unwrap();

        assert!(dest.is_dir());
        assert!(!is_symlink(&dest));
        assert!(dest.join("file.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_restore_dir_permission_denied() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("backup");
        let dest = dir.path().join("restore_target");

        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("old.txt"), "old").unwrap();

        fs::set_permissions(&dest, Permissions::from_mode(0o000)).unwrap();

        let action = Action::RestoreDir {
            source: source.clone(),
            dest: dest.clone(),
        };
        let result = action_execute(
            &action,
            &mut RepoState::new_for_git(PathBuf::from("."), PathBuf::from(".")),
        );

        assert!(result.is_err());
    }

    // -------------------------------------------------------------------
    // execute_plan rollback tests
    // -------------------------------------------------------------------

    #[test]
    fn test_execute_plan_mid_plan_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        fs::create_dir_all(&state).unwrap();

        let dir_a = dir.path().join("dir_a");
        let dir_b = dir.path().join("dir_b");

        let mut plan = Plan::new(dir.path());
        plan.add(Action::CreateDir {
            path: dir_a.clone(),
        });
        plan.add(Action::CreateDir {
            path: dir_b.clone(),
        });
        plan.add(Action::Backup {
            source: dir.path().join("nonexistent"),
            dest: dir.path().join("backup_dest"),
        });

        let result = execute_plan(
            &plan,
            ExecuteMode::Normal,
            &mut RepoState::new_for_git(dir.path().to_path_buf(), state),
        );

        assert!(
            result.is_err(),
            "Plan should fail due to missing backup source"
        );
        assert!(!dir_a.exists(), "dir_a should be rolled back");
        assert!(!dir_b.exists(), "dir_b should be rolled back");
    }

    /// Test that RestoreBackup rollback propagates error when the destination
    /// parent directory is not writable, simulating a partial rollback failure.
    #[cfg(unix)]
    #[test]
    fn test_rollback_failure_on_readonly_parent() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        let backup = dir.path().join("backup.txt");

        fs::write(&target, "target content").unwrap();
        fs::write(&backup, "backup content").unwrap();

        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
            backup_path: Some(backup.clone()),
        };
        let mut repo_state = RepoState::new_for_git(PathBuf::from("."), PathBuf::from("."));
        action_execute(&action, &mut repo_state).unwrap();
        assert!(is_symlink(&link));

        // Make link's parent read-only so that rollback's copy_file fails
        fs::set_permissions(dir.path(), Permissions::from_mode(0o555)).unwrap();

        let rollback_action = action_rollback(&action).unwrap();
        let result = action_execute(&rollback_action, &mut repo_state);

        assert!(
            result.is_err(),
            "Rollback should fail when dest parent is read-only"
        );
    }

    /// Test that rolling back a plan with multiple GitCommit actions resets
    /// to HEAD~N (N = number of commits), not HEAD~1 per commit.
    #[test]
    fn test_rollback_multiple_git_commits() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let state = dir.path().join("state");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();

        // Init git repo with initial commit
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["init", "-q"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();
        fs::write(repo.join("file.txt"), "initial").unwrap();
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["add", "file.txt"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-m", "initial", "-q"])
            .output()
            .unwrap();

        let initial_count: usize = git::git_run(&repo, &["rev-list", "--count", "HEAD"])
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        // Plan: 2 GitCommit actions followed by a failing Backup
        let mut plan = Plan::new(&repo);
        plan.add(Action::GitCommit {
            message: "commit one".to_string(),
        });
        plan.add(Action::GitCommit {
            message: "commit two".to_string(),
        });
        // This action will fail, triggering rollback of the 2 commits
        plan.add(Action::Backup {
            source: repo.join("nonexistent"),
            dest: repo.join("backup"),
        });

        // Modify file so commits have something to commit
        fs::write(repo.join("file.txt"), "v1").unwrap();
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["add", "file.txt"])
            .output()
            .unwrap();

        let mut repo_state = RepoState::new_for_git(repo.clone(), state.clone());

        let result = execute_plan(&plan, ExecuteMode::Rollback, &mut repo_state);

        assert!(
            result.is_err(),
            "Plan should fail due to missing backup source"
        );

        // After rollback, should be back to initial commit count
        let final_count: usize = git::git_run(&repo, &["rev-list", "--count", "HEAD"])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            final_count,
            initial_count,
            "Rollback should undo all {} commits, restoring to initial state",
            plan.actions
                .iter()
                .filter(|a| matches!(a, Action::GitCommit { .. }))
                .count()
        );
    }

    // -------------------------------------------------------------------
    // Per-variant unit tests for extracted functions
    // -------------------------------------------------------------------

    #[test]
    fn test_exec_create_dir_creates_nested() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c");
        exec_create_dir(&path).unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn test_exec_create_dir_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing");
        fs::create_dir_all(&path).unwrap();
        exec_create_dir(&path).unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn test_exec_remove_file_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");
        exec_remove_file(&path).unwrap();
    }

    #[test]
    fn test_exec_remove_file_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target.txt");
        fs::write(&path, "data").unwrap();
        exec_remove_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_exec_remove_dir_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent_dir");
        exec_remove_dir(&path).unwrap();
    }

    #[test]
    fn test_exec_remove_dir_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target_dir");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("inner.txt"), "data").unwrap();
        exec_remove_dir(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_exec_orphan_removed_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ghost");
        exec_orphan_removed(&path).unwrap();
    }

    #[test]
    fn test_exec_abort_gate_non_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            exec_abort_gate("continue?").unwrap();
        });
    }
}
