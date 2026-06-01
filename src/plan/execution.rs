//! Plan execution and rollback logic.
//!
//! Contains [`execute_plan`], the [`RollbackAction`] enum, and helper functions
//! for executing individual actions, rolling back completed actions, and
//! copying/verifying files.

use std::fs;
use std::io;
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
        Action::CreateDir { path } => {
            fs::create_dir_all(path).map_err(|e| io_error_with_path(e, path))?;
        }
        Action::Backup { source, dest } => {
            let parent = dest.parent().ok_or_else(|| DottyError::PathResolution {
                path: dest.to_path_buf(),
                reason: format!("cannot determine parent of backup path: {}", dest.display()),
            })?;
            fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
            copy_file(source, dest)?;
            verify_backup_integrity(source, dest)?;
        }
        Action::BackupDir {
            source,
            dest,
            follow_symlinks,
        } => {
            let parent = dest.parent().ok_or_else(|| DottyError::PathResolution {
                path: dest.to_path_buf(),
                reason: format!(
                    "cannot determine parent of backup dir path: {}",
                    dest.display()
                ),
            })?;
            fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
            copy_dir(source, dest, *follow_symlinks)?;
        }
        Action::CopyFile { source, dest } => {
            let parent = dest.parent();
            if let Some(p) = parent {
                fs::create_dir_all(p).map_err(|e| io_error_with_path(e, p))?;
            }
            // Remove existing symlink so fs::copy creates a regular file
            // (fs::copy follows symlinks, writing to the target instead of
            // replacing the symlink itself).
            if is_symlink(dest) {
                fs::remove_file(dest).map_err(|e| io_error_with_path(e, dest))?;
            }
            copy_file(source, dest)?;
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
                // Warn if the symlink points to a directory — the target
                // directory content will be orphaned (not touched, but no
                // longer managed by dotty).
                if let Ok(target) = fs::read_link(path)
                    && target.is_dir()
                {
                    warn!(
                        "Removing symlink to directory: {} → {} (directory content preserved)",
                        path.display(),
                        target.display()
                    );
                }
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
            copy_file(source, dest)?;
        }
        Action::RestoreDir { source, dest } => {
            // Remove existing directory (if any) before restoring backup
            if dest.exists() {
                if dest.is_dir() && !is_symlink(dest) {
                    fs::remove_dir_all(dest).map_err(|e| io_error_with_path(e, dest))?;
                } else if is_symlink(dest) {
                    fs::remove_file(dest).map_err(|e| io_error_with_path(e, dest))?;
                }
            }
            // RestoreDir always follows symlinks: the backup was created
            // with the actual content, so we restore it faithfully.
            copy_dir(source, dest, true)?;
        }
        Action::GitAdd { paths } => git::git_add(&repo_state.repo_path, paths)?,
        Action::GitCommit { message } => git::git_commit(repo_state, message)?,
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
        Action::BackupDir { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::CopyFile { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::CreateSymlink {
            link,
            backup_path,
            backup_exists,
            target: _,
        } => {
            // Use the stored backup_exists flag (recorded at plan construction time)
            // instead of checking backup.exists() here, which would be a TOCTOU race:
            // the backup could be deleted between execution and rollback (e.g., by
            // a concurrent `dotty clean`), causing silent data loss.
            if let Some(backup) = backup_path {
                if *backup_exists {
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
        Action::RestoreDir { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
        Action::GitAdd { .. } => None,
        Action::GitCommit { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Plan execution
// ---------------------------------------------------------------------------

/// Execution mode for [`execute_plan`].
///
/// Controls whether the plan is a dry-run, whether a pending plan is saved
/// for crash recovery, and whether it is cleared on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecuteMode {
    /// Normal execution: save pending plan, clear on success.
    Normal,
    /// Dry-run: no mutations, no pending plan.
    DryRun,
    /// Rollback: no pending plan (avoids nested pending plans).
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

/// Execute all actions in the plan.
///
/// The `mode` parameter controls execution behavior:
/// - [`ExecuteMode::Normal`] — save pending plan for crash recovery, clear on success.
/// - [`ExecuteMode::DryRun`] — print actions, perform no mutations.
/// - [`ExecuteMode::Rollback`] — execute without saving pending plan to avoid
///   nested pending plans (used by crash recovery rollback).
///
/// If any action fails, roll back all previously completed actions in
/// reverse order.
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
        println!("[dry-run] Plan ({} actions):", plan.actions.len());
        for (i, action) in plan.actions.iter().enumerate() {
            println!("[dry-run]  {}. {}", i + 1, action);
        }
        println!("[dry-run] no changes made");
        return Ok(());
    }

    // Save pending plan for crash recovery (skipped for rollback/dry-run)
    if mode.save_pending() {
        crate::plan::save_pending_plan(plan, &repo_state.state_path)?;
    }

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
        match action_execute(action, repo_state) {
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
    fn execute(&self, repo_state: &mut RepoState) -> Result<(), DottyError> {
        match self {
            RollbackAction::Filesystem(action) => action_execute(action, repo_state),
            RollbackAction::GitResetSoft => git::git_reset_soft_head(&repo_state.repo_path),
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
fn rollback_completed(
    plan: &super::Plan,
    completed_indices: &[usize],
    repo_state: &mut RepoState,
) -> Result<(), DottyError> {
    debug!("rolling back {} completed actions", completed_indices.len());
    let actions = &plan.actions;
    let _repo_path = &plan.repo_path;

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
    fs::copy(source, dest).map(|_| ())?;
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
    crate::fs_utils::walk_dir(source, &mut files, 0)?;

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
                reason: format!("cannot strip source prefix: {e}"),
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
    let mut file = fs::File::open(path).map_err(DottyError::Io)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(DottyError::Io)?;
    let hash = hasher.finalize();
    Ok(format!("{:x}", hash))
}

/// Verify that a backup file was created correctly.
///
/// Uses a two-tier verification strategy for performance:
/// - Files ≤ 1KB: size check only (fast, catches most corruption)
/// - Files > 1KB: size check + SHA-256 hash verification (strong integrity)
///
/// The 1KB threshold balances security for critical dotfiles (SSH keys, GPG
/// configs) against performance for many small config files.
pub(crate) fn verify_backup_integrity(source: &Path, dest: &Path) -> Result<(), DottyError> {
    // Fast pre-validation: check file sizes match
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

    // SHA-256 verification for files > 1KB (performance tradeoff)
    // Small files use size-only check; larger files get cryptographic verification
    const HASH_VERIFICATION_THRESHOLD: u64 = 1024; // 1KB
    if source_size > HASH_VERIFICATION_THRESHOLD {
        let source_hash = compute_file_hash(source)?;
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

        let result = verify_backup_integrity(&source, &dest);
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

        let result = verify_backup_integrity(&source, &dest);
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

        let result = verify_backup_integrity(&source, &dest);
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

        let result = verify_backup_integrity(&source, &dest);
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

        let result = verify_backup_integrity(&source, &dest);
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

        let result = verify_backup_integrity(&source, &dest);
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

        let result = verify_backup_integrity(&source, &dest);
        assert!(
            result.is_ok(),
            "1KB+1 file should pass with hash verification"
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
}
