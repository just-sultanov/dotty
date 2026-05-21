//! Plan execution types: [`Action`] and [`Plan`].
//!
//! An [`Action`] represents a single atomic filesystem or git operation that can
//! be executed and rolled back. A [`Plan`] is a sequence of actions, built in a
//! pure phase (no side effects), then executed with automatic rollback on failure.
//!
//! ## Module structure
//!
//! - This module (`plan`) — [`Action`] enum and [`Plan`] struct.
//! - [`execution`] — plan execution, rollback logic, and progress bar.
//! - [`persistence`] — pending plan save/load/clear for crash recovery.

mod execution;
mod persistence;

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) use execution::{ExecuteMode, execute_plan};

#[cfg(test)]
pub(crate) use execution::action_execute;
pub(crate) use persistence::{clear_pending_plan, load_pending_plan, save_pending_plan};

#[cfg(test)]
pub(crate) use persistence::PendingPlan;

/// Maximum number of paths to show in `GitAdd` action display.
const GIT_ADD_MAX_SHOWN: usize = 3;

// ---------------------------------------------------------------------------
// Action enum
// ---------------------------------------------------------------------------

/// A single atomic operation within a plan.
///
/// Each action can be executed and, if needed, rolled back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Action {
    CreateDir {
        path: PathBuf,
    },
    Backup {
        source: PathBuf,
        dest: PathBuf,
    },
    /// Recursively copy a directory to a backup destination.
    /// Used when replacing an existing directory with a symlink.
    ///
    /// When `follow_symlinks` is false (default), symlinked files are skipped
    /// during the backup to prevent exposing sensitive data outside the
    /// intended home directory. When true, symlinks are dereferenced and their
    /// target content is copied instead.
    BackupDir {
        source: PathBuf,
        dest: PathBuf,
        follow_symlinks: bool,
    },
    CopyFile {
        source: PathBuf,
        dest: PathBuf,
    },
    /// Replaces a file or directory at `link` with a symlink to `target`.
    ///
    /// `backup_path` records where the original content was saved (if any).
    /// `backup_exists` records whether that backup file actually existed at
    /// plan construction time. This avoids a TOCTOU race in `action_rollback`
    /// where `backup.exists()` could return false at rollback time even though
    /// the backup existed when the plan was built (e.g., if a concurrent
    /// `dotty clean` deleted it), which would cause silent data loss — the
    /// rollback would remove the symlink without restoring the original file.
    CreateSymlink {
        target: PathBuf,
        link: PathBuf,
        backup_path: Option<PathBuf>,
        backup_exists: bool,
    },
    RemoveFile {
        path: PathBuf,
    },
    RemoveSymlink {
        path: PathBuf,
    },
    RestoreBackup {
        source: PathBuf,
        dest: PathBuf,
    },
    /// Recursively copy a backup directory back to its original location.
    /// Used for rolling back a directory-to-symlink replacement.
    RestoreDir {
        source: PathBuf,
        dest: PathBuf,
    },
    GitAdd {
        paths: Vec<PathBuf>,
    },
    GitCommit {
        message: String,
    },
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::CreateDir { path } => write!(f, "create dir    {}", path.display()),
            Action::Backup { source, dest } => {
                write!(f, "backup        {} → {}", source.display(), dest.display())
            }
            Action::BackupDir { source, dest, .. } => {
                write!(f, "backup dir    {} → {}", source.display(), dest.display())
            }
            Action::CopyFile { source, dest } => {
                write!(f, "copy file     {} → {}", source.display(), dest.display())
            }
            Action::CreateSymlink { target, link, .. } => {
                write!(f, "create link   {} → {}", link.display(), target.display())
            }
            Action::RemoveFile { path } => write!(f, "remove file   {}", path.display()),
            Action::RemoveSymlink { path } => write!(f, "remove link   {}", path.display()),
            Action::RestoreBackup { source, dest } => {
                write!(
                    f,
                    "restore backup {} → {}",
                    source.display(),
                    dest.display()
                )
            }
            Action::RestoreDir { source, dest } => {
                write!(
                    f,
                    "restore dir     {} → {}",
                    source.display(),
                    dest.display()
                )
            }
            Action::GitAdd { paths } => {
                if paths.is_empty() {
                    return write!(f, "git add       (empty)");
                }
                write!(f, "git add       {}", paths[0].display())?;
                for p in paths.iter().skip(1).take(GIT_ADD_MAX_SHOWN - 1) {
                    write!(f, ", {}", p.display())?;
                }
                if paths.len() > GIT_ADD_MAX_SHOWN {
                    write!(f, " (+{} more)", paths.len() - GIT_ADD_MAX_SHOWN)?;
                }
                Ok(())
            }
            Action::GitCommit { message } => write!(f, "git commit    {message}"),
        }
    }
}

impl Action {
    /// Return the inverse filesystem action, or `None` if not reversible.
    ///
    /// Filesystem actions (CreateDir, Backup, CopyFile, CreateSymlink) are
    /// reversible. RemoveFile / RemoveSymlink return None because the original
    /// content is not tracked (the file was already removed from management;
    /// to restore it, the user would need to re-add it or use `git checkout`).
    /// Git actions (GitAdd, GitCommit) are handled separately in
    /// `rollback_completed` via `git reset`.
    pub fn rollback(&self) -> Option<Action> {
        execution::action_rollback(self)
    }
}

// ---------------------------------------------------------------------------
// Plan struct
// ---------------------------------------------------------------------------

/// A plan is a sequence of actions to be executed together.
///
/// Built in a pure phase (no side effects), then executed with automatic
/// rollback on failure.
#[derive(Debug)]
pub(crate) struct Plan {
    pub repo_path: PathBuf,
    pub actions: Vec<Action>,
}

impl Plan {
    /// Create a new empty plan.
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            actions: Vec::new(),
        }
    }

    /// Add an action to the plan.
    pub fn add(&mut self, action: Action) {
        self.actions.push(action);
    }

    /// Check if the plan has no actions (nothing to do).
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::ExecuteMode;
    use super::*;
    use crate::repo_state::RepoState;

    /// Create a unique temporary directory that is automatically cleaned up on drop.
    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Initialize a real git repo at the given path.
    fn init_git_repo(path: &Path) {
        std::process::Command::new("git")
            .current_dir(path)
            .args(["init"])
            .output()
            .expect("git init should work in test env");
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = test_dir();
        let path = dir.path().to_path_buf();
        std::fs::create_dir_all(&path).unwrap();
        init_git_repo(&path);
        (dir, path)
    }

    fn dummy_repo_path() -> PathBuf {
        PathBuf::from(".")
    }

    #[test]
    fn test_create_dir_action() {
        let (_dir, base) = setup();
        let path = base.join("new_dir/nested");

        let action = Action::CreateDir { path: path.clone() };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn test_copy_file_action() {
        let (_dir, base) = setup();
        let src = base.join("source.txt");
        let dst = base.join("dest.txt");

        std::fs::write(&src, "hello world").unwrap();

        let action = Action::CopyFile {
            source: src.clone(),
            dest: dst.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(dst.exists());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "hello world");
    }

    #[test]
    fn test_copy_file_creates_parent_dirs() {
        let (_dir, base) = setup();
        let src = base.join("source.txt");
        let dst = base.join("a/b/c/dest.txt");

        std::fs::write(&src, "data").unwrap();

        let action = Action::CopyFile {
            source: src,
            dest: dst.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(dst.exists());
    }

    #[test]
    fn test_backup_action() {
        let (_dir, base) = setup();
        let src = base.join("original.txt");
        let backup_dir = base.join("backups/2024-01-01T00-00-00");
        let dst = backup_dir.join("original.txt");

        std::fs::write(&src, "original content").unwrap();

        let action = Action::Backup {
            source: src,
            dest: dst.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(dst.exists());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "original content");
    }

    #[test]
    fn test_backup_dir_action() {
        let (_dir, base) = setup();
        let src = base.join("source_dir");
        let backup_dir = base.join("backups/2024-01-01T00-00-00");

        // Create a directory with nested files
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("file1.txt"), "content1").unwrap();
        std::fs::write(src.join("sub").join("file2.txt"), "content2").unwrap();

        let action = Action::BackupDir {
            source: src.clone(),
            dest: backup_dir.clone(),
            follow_symlinks: false,
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        // Verify the backup directory exists with all files
        assert!(backup_dir.exists());
        assert!(backup_dir.is_dir());
        assert!(backup_dir.join("file1.txt").exists());
        assert!(backup_dir.join("sub").is_dir());
        assert!(backup_dir.join("sub").join("file2.txt").exists());
        assert_eq!(
            std::fs::read_to_string(backup_dir.join("file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            std::fs::read_to_string(backup_dir.join("sub").join("file2.txt")).unwrap(),
            "content2"
        );
    }

    #[test]
    fn test_restore_dir_action() {
        let (_dir, base) = setup();
        let backup = base.join("backups/2024-01-01T00-00-00");
        let dest = base.join("restored_dir");

        // Create a backup directory
        std::fs::create_dir_all(backup.join("sub")).unwrap();
        std::fs::write(backup.join("file1.txt"), "content1").unwrap();
        std::fs::write(backup.join("sub").join("file2.txt"), "content2").unwrap();

        // Create a directory at the destination (simulating the state before restore)
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("old.txt"), "old").unwrap();

        let action = Action::RestoreDir {
            source: backup.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        // Verify the backup was restored (old directory replaced)
        assert!(dest.exists());
        assert!(dest.is_dir());
        assert!(dest.join("file1.txt").exists());
        assert!(dest.join("sub").is_dir());
        assert!(dest.join("sub").join("file2.txt").exists());
        assert!(!dest.join("old.txt").exists()); // old file should be gone
        assert_eq!(
            std::fs::read_to_string(dest.join("file1.txt")).unwrap(),
            "content1"
        );
    }

    #[test]
    fn test_rollback_backup_dir() {
        let (_dir, base) = setup();
        let backup_dir = base.join("backups/2024-01-01T00-00-00");
        let src = base.join("source");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("file.txt"), "data").unwrap();

        let action = Action::BackupDir {
            source: src,
            dest: backup_dir.clone(),
            follow_symlinks: false,
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(backup_dir.exists());

        // Rollback of BackupDir is RemoveFile (removes the backup)
        let rollback = action.rollback().unwrap();
        match &rollback {
            Action::RemoveFile { path } => {
                assert_eq!(path, &backup_dir);
            }
            other => panic!("expected RemoveFile, got {:?}", other),
        }
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(!backup_dir.exists());
    }

    #[test]
    fn test_rollback_restore_dir() {
        let (_dir, base) = setup();
        let dest = base.join("restored_dir");
        let backup = base.join("backups/2024-01-01T00-00-00");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("file.txt"), "data").unwrap();

        let action = Action::RestoreDir {
            source: backup,
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(dest.exists());

        // Rollback of RestoreDir is RemoveFile (removes the restored dir)
        let rollback = action.rollback().unwrap();
        match &rollback {
            Action::RemoveFile { path } => {
                assert_eq!(path, &dest);
            }
            other => panic!("expected RemoveFile, got {:?}", other),
        }
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(!dest.exists());
    }

    #[test]
    fn test_remove_file_action() {
        let (_dir, base) = setup();
        let path = base.join("to_remove.txt");
        std::fs::write(&path, "delete me").unwrap();

        let action = Action::RemoveFile { path: path.clone() };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_remove_file_idempotent() {
        let (_dir, base) = setup();
        let path = base.join("does_not_exist.txt");

        let action = Action::RemoveFile { path };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
    }

    #[test]
    fn test_create_symlink_action() {
        let (_dir, base) = setup();
        let target = base.join("real_file.txt");
        let link = base.join("link_to_file");

        std::fs::write(&target, "content").unwrap();

        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
            backup_path: None,
            backup_exists: false,
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(crate::symlink::is_symlink(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn test_create_symlink_replaces_existing() {
        let (_dir, base) = setup();
        let target1 = base.join("file1.txt");
        let target2 = base.join("file2.txt");
        let link = base.join("link");

        std::fs::write(&target1, "one").unwrap();
        std::fs::write(&target2, "two").unwrap();

        action_execute(
            &Action::CreateSymlink {
                target: target1.clone(),
                link: link.clone(),
                backup_path: None,
                backup_exists: false,
            },
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        action_execute(
            &Action::CreateSymlink {
                target: target2.clone(),
                link: link.clone(),
                backup_path: None,
                backup_exists: false,
            },
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        assert!(crate::symlink::is_symlink(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), target2);
    }

    /// Verify that CreateSymlink replaces an existing directory with a symlink.
    ///
    /// This tests the Windows bug scenario where a real directory exists at the
    /// link path and must be replaced with a symlink to a directory target.
    /// On Windows, this requires `symlink_dir` (junction) instead of `symlink_file`.
    #[test]
    fn test_create_symlink_replaces_existing_directory_with_dir_target() {
        let (_dir, base) = setup();
        let target_dir = base.join("target_dir");
        let link = base.join("link_to_dir");

        // Create a real directory at the link location
        std::fs::create_dir(&link).unwrap();
        assert!(link.is_dir());
        assert!(!crate::symlink::is_symlink(&link));

        // Create the actual target directory
        std::fs::create_dir(&target_dir).unwrap();

        // CreateSymlink should remove the existing directory and create a symlink
        action_execute(
            &Action::CreateSymlink {
                target: target_dir.clone(),
                link: link.clone(),
                backup_path: None,
                backup_exists: false,
            },
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        assert!(crate::symlink::is_symlink(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), target_dir);
    }

    #[test]
    fn test_rollback_create_dir() {
        let (_dir, base) = setup();
        let path = base.join("rollback_dir");

        let action = Action::CreateDir { path: path.clone() };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(path.is_dir());

        let rollback = action.rollback().unwrap();
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_rollback_copy_file() {
        let (_dir, base) = setup();
        let src = base.join("src.txt");
        let dst = base.join("dst.txt");

        std::fs::write(&src, "data").unwrap();

        let action = Action::CopyFile {
            source: src,
            dest: dst.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(dst.exists());

        let rollback = action.rollback().unwrap();
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(!dst.exists());
    }

    #[test]
    fn test_rollback_symlink() {
        let (_dir, base) = setup();
        let target = base.join("target.txt");
        let link = base.join("link");

        std::fs::write(&target, "content").unwrap();

        let action = Action::CreateSymlink {
            target,
            link: link.clone(),
            backup_path: None,
            backup_exists: false,
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(crate::symlink::is_symlink(&link));

        let rollback = action.rollback().unwrap();
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(!crate::symlink::is_symlink(&link));
        assert!(!link.exists());
    }

    #[test]
    fn test_plan_empty() {
        let plan = Plan::new(&dummy_repo_path());
        assert!(plan.is_empty());
    }

    #[test]
    fn test_plan_add_actions() {
        let mut plan = Plan::new(&dummy_repo_path());
        plan.add(Action::CreateDir {
            path: PathBuf::from("/tmp/test"),
        });
        plan.add(Action::CopyFile {
            source: PathBuf::from("/tmp/a"),
            dest: PathBuf::from("/tmp/b"),
        });
        assert_eq!(plan.actions.len(), 2);
        assert!(!plan.is_empty());
    }

    #[test]
    fn test_execute_plan_dry_run() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("should_not_exist"),
        });

        execute_plan(
            &plan,
            ExecuteMode::DryRun,
            &mut RepoState::new_for_git(dummy_repo_path(), state.clone()),
        )
        .unwrap();
        assert!(!base.join("should_not_exist").exists());
    }

    #[test]
    fn test_execute_plan_empty() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let plan = Plan::new(&base);
        execute_plan(
            &plan,
            ExecuteMode::Normal,
            &mut RepoState::new_for_git(dummy_repo_path(), state.clone()),
        )
        .unwrap();
    }

    #[test]
    fn test_action_display() {
        let action = Action::CreateDir {
            path: PathBuf::from("/tmp/test"),
        };
        let display = format!("{}", action);
        assert!(display.contains("create dir"));
        assert!(display.contains("/tmp/test"));

        let action = Action::GitCommit {
            message: "add vimrc".to_string(),
        };
        let display = format!("{}", action);
        assert!(display.contains("git commit"));
        assert!(display.contains("add vimrc"));
    }

    #[test]
    fn test_copy_file_follows_symlinks() {
        let (_dir, base) = setup();
        let real = base.join("real.txt");
        let sym = base.join("sym.txt");
        let dst = base.join("copied.txt");

        std::fs::write(&real, "real content").unwrap();
        crate::symlink::create_symlink(&real, &sym).unwrap();

        execution::copy_file(&sym, &dst).unwrap();
        assert!(!crate::symlink::is_symlink(&dst));
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "real content");
    }

    #[test]
    fn test_backup_verification_success() {
        let (_dir, base) = setup();
        let src = base.join("source.txt");
        let dst = base.join("backup.txt");

        std::fs::write(&src, "original content").unwrap();
        std::fs::write(&dst, "original content").unwrap();

        execution::verify_backup_integrity(&src, &dst).unwrap();
    }

    #[test]
    fn test_backup_verification_size_mismatch() {
        let (_dir, base) = setup();
        let src = base.join("source.txt");
        let dst = base.join("backup.txt");

        std::fs::write(&src, "original content").unwrap();
        std::fs::write(&dst, "short").unwrap();

        let result = execution::verify_backup_integrity(&src, &dst);
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::DottyError::BackupVerification { path, detail } => {
                assert_eq!(path, dst);
                assert!(detail.contains("size mismatch"));
            }
            other => panic!("expected BackupVerification error, got: {other}"),
        }
    }

    #[test]
    fn test_backup_verification_missing_backup() {
        let (_dir, base) = setup();
        let src = base.join("source.txt");
        let dst = base.join("backup_missing.txt");

        std::fs::write(&src, "content").unwrap();
        // dst does not exist

        let result = execution::verify_backup_integrity(&src, &dst);
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::DottyError::BackupVerification { path, detail } => {
                assert_eq!(path, dst);
                assert!(detail.contains("does not exist") || detail.contains("not readable"));
            }
            other => panic!("expected BackupVerification error, got: {other}"),
        }
    }

    #[test]
    fn test_backup_verification_empty_files() {
        let (_dir, base) = setup();
        let src = base.join("empty.txt");
        let dst = base.join("empty_backup.txt");

        std::fs::write(&src, "").unwrap();
        std::fs::write(&dst, "").unwrap();

        // Two empty files should pass verification (both 0 bytes)
        execution::verify_backup_integrity(&src, &dst).unwrap();
    }

    #[test]
    fn test_backup_action_with_verification() {
        let (_dir, base) = setup();
        let src = base.join("original.txt");
        let backup_dir = base.join("backups/2024-01-01T00-00-00");
        let dst = backup_dir.join("original.txt");

        std::fs::write(&src, "original content").unwrap();

        let action = Action::Backup {
            source: src,
            dest: dst.clone(),
        };
        // Should succeed: copy + verify
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(dst.exists());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "original content");
    }

    // -- pending plan tests --

    #[test]
    fn test_save_and_load_pending_plan() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("new_dir"),
        });
        plan.add(Action::CopyFile {
            source: base.join("src.txt"),
            dest: base.join("dst.txt"),
        });

        save_pending_plan(&plan, &state).unwrap();

        // Verify file exists
        assert!(state.join("pending_plan.json").exists());

        // Load and verify
        let loaded = load_pending_plan(&state).unwrap();
        assert!(loaded.is_some());
        let loaded_plan = loaded.unwrap();
        assert_eq!(loaded_plan.actions.len(), 2);
        assert_eq!(loaded_plan.repo_path, base);
    }

    #[test]
    fn test_load_pending_plan_missing() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let loaded = load_pending_plan(&state).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_clear_pending_plan() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("dir"),
        });

        save_pending_plan(&plan, &state).unwrap();
        assert!(state.join("pending_plan.json").exists());

        clear_pending_plan(&state).unwrap();
        assert!(!state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_clear_pending_plan_idempotent() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        // Clearing when no file exists should not error
        clear_pending_plan(&state).unwrap();
        clear_pending_plan(&state).unwrap();
    }

    #[test]
    fn test_pending_plan_roundtrip_all_action_types() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("dir"),
        });
        plan.add(Action::Backup {
            source: base.join("src.txt"),
            dest: base.join("backup.txt"),
        });
        plan.add(Action::CopyFile {
            source: base.join("a.txt"),
            dest: base.join("b.txt"),
        });
        plan.add(Action::CreateSymlink {
            target: base.join("target"),
            link: base.join("link"),
            backup_path: None,
            backup_exists: false,
        });
        plan.add(Action::RemoveFile {
            path: base.join("remove.txt"),
        });
        plan.add(Action::RemoveSymlink {
            path: base.join("remove_link"),
        });
        plan.add(Action::GitAdd {
            paths: vec![base.join("file1.txt"), base.join("file2.txt")],
        });
        plan.add(Action::GitCommit {
            message: "test commit".to_string(),
        });

        save_pending_plan(&plan, &state).unwrap();
        let loaded = load_pending_plan(&state).unwrap().unwrap();

        assert_eq!(loaded.actions.len(), 8);

        // Verify each action type roundtrips correctly
        match &loaded.actions[0] {
            Action::CreateDir { path } => assert!(path.ends_with("dir")),
            other => panic!("expected CreateDir, got {:?}", other),
        }
        match &loaded.actions[7] {
            Action::GitCommit { message } => assert_eq!(message, "test commit"),
            other => panic!("expected GitCommit, got {:?}", other),
        }
    }

    #[test]
    fn test_execute_plan_saves_and_clears_pending() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("test_dir"),
        });

        // Before execution, no pending plan
        assert!(!state.join("pending_plan.json").exists());

        // Execute plan
        execute_plan(
            &plan,
            ExecuteMode::Normal,
            &mut RepoState::new_for_git(dummy_repo_path(), state.clone()),
        )
        .unwrap();

        // After successful execution, pending plan is cleared
        assert!(!state.join("pending_plan.json").exists());
        assert!(base.join("test_dir").is_dir());
    }

    #[test]
    fn test_execute_plan_dry_run_does_not_save_pending() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("should_not_exist"),
        });

        execute_plan(
            &plan,
            ExecuteMode::DryRun,
            &mut RepoState::new_for_git(dummy_repo_path(), state.clone()),
        )
        .unwrap();

        // Dry run should not create pending plan file
        assert!(!state.join("pending_plan.json").exists());
        assert!(!base.join("should_not_exist").exists());
    }

    #[test]
    fn test_execute_plan_with_many_actions_uses_progress_bar() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        // Add 25 actions (>20 threshold) to trigger progress bar path
        for i in 0..25 {
            plan.add(Action::CreateDir {
                path: base.join(format!("dir_{i}")),
            });
        }

        execute_plan(
            &plan,
            ExecuteMode::Normal,
            &mut RepoState::new_for_git(dummy_repo_path(), state.clone()),
        )
        .unwrap();

        // All directories should be created
        for i in 0..25 {
            assert!(base.join(format!("dir_{i}")).is_dir());
        }
        // Pending plan should be cleared after success
        assert!(!state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_execute_plan_with_exactly_20_actions_no_progress_bar() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        // Exactly 20 actions — at the threshold, should NOT use progress bar
        for i in 0..20 {
            plan.add(Action::CreateDir {
                path: base.join(format!("dir_{i}")),
            });
        }

        execute_plan(
            &plan,
            ExecuteMode::Normal,
            &mut RepoState::new_for_git(dummy_repo_path(), state.clone()),
        )
        .unwrap();

        for i in 0..20 {
            assert!(base.join(format!("dir_{i}")).is_dir());
        }
    }

    // -- pending plan integrity validation tests --

    #[test]
    fn test_load_pending_plan_validates_repo_exists() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        // Create a separate repo directory (not under base which has .git)
        let outer = tempfile::tempdir().unwrap();
        let repo_dir = outer.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir);

        let mut plan = Plan::new(&repo_dir);
        plan.add(Action::CreateDir {
            path: repo_dir.join("dir"),
        });
        save_pending_plan(&plan, &state).unwrap();

        // Should load successfully (repo + .git exist)
        let loaded = load_pending_plan(&state).unwrap();
        assert!(loaded.is_some());

        // Now remove the repo directory
        std::fs::remove_dir_all(&repo_dir).unwrap();

        // Should return PendingPlanInvalid
        let result = load_pending_plan(&state);
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::DottyError::PendingPlanInvalid { reason, .. } => {
                assert!(reason.contains("no longer exists"));
            }
            other => panic!("expected PendingPlanInvalid, got: {other}"),
        }
    }

    #[test]
    fn test_load_pending_plan_validates_git_directory() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        // Create a directory that exists but is NOT a git repo
        let fake_repo = base.join("fake_repo");
        std::fs::create_dir_all(&fake_repo).unwrap();
        // No .git directory

        let mut plan = Plan::new(&fake_repo);
        plan.add(Action::CreateDir {
            path: fake_repo.join("dir"),
        });
        save_pending_plan(&plan, &state).unwrap();

        // Should return PendingPlanInvalid because .git is missing
        let result = load_pending_plan(&state);
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::DottyError::PendingPlanInvalid { reason, .. } => {
                assert!(reason.contains("not a git repository"));
            }
            other => panic!("expected PendingPlanInvalid, got: {other}"),
        }
    }

    #[test]
    fn test_load_pending_plan_valid_with_git_dir() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        // Create a valid git repo
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let mut plan = Plan::new(&repo);
        plan.add(Action::CreateDir {
            path: repo.join("dir"),
        });
        save_pending_plan(&plan, &state).unwrap();

        // Should load successfully
        let loaded = load_pending_plan(&state).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().repo_path, repo);
    }

    #[test]
    fn test_load_pending_plan_rejects_corrupted_git_repo() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        // Create a corrupted repo in a separate tempdir (not under base, which is a git repo).
        // git rev-parse --git-dir climbs the directory tree, so the corrupted repo must be
        // outside any parent git repository to ensure the check properly detects corruption.
        let outer = tempfile::tempdir().unwrap();
        let fake_repo = outer.path().join("fake_repo");
        std::fs::create_dir_all(&fake_repo).unwrap();
        init_git_repo(&fake_repo);
        // Corrupt: remove HEAD so git rev-parse fails
        std::fs::remove_file(fake_repo.join(".git").join("HEAD")).unwrap();

        let mut plan = Plan::new(&fake_repo);
        plan.add(Action::CreateDir {
            path: fake_repo.join("dir"),
        });
        save_pending_plan(&plan, &state).unwrap();

        // Should return PendingPlanInvalid because git rev-parse fails
        let result = load_pending_plan(&state);
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::DottyError::PendingPlanInvalid { reason, .. } => {
                assert!(reason.contains("corrupted"));
            }
            other => panic!("expected PendingPlanInvalid, got: {other}"),
        }
    }

    // -- CreateSymlink rollback with backup restoration tests --

    /// Test that rolling back CreateSymlink restores the original file
    /// from backup when a backup path is provided and the backup exists.
    #[test]
    fn test_rollback_symlink_restores_backup_when_exists() {
        let (_dir, base) = setup();
        let target = base.join("repo_file.txt");
        let link = base.join("link");
        let backup = base.join("backups/original.txt");

        // Create the repo file (symlink target)
        std::fs::write(&target, "repo content").unwrap();
        // Create the backup (original file content saved before symlink)
        std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
        std::fs::write(&backup, "original content").unwrap();

        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
            backup_path: Some(backup.clone()),
            backup_exists: true,
        };

        // Execute: creates symlink at link → target
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(crate::symlink::is_symlink(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), target);

        // Rollback: should restore backup to link location
        let rollback = action.rollback().unwrap();
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        // Symlink should be gone, original content restored
        assert!(!crate::symlink::is_symlink(&link));
        assert!(link.exists());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "original content");
    }

    /// Test that rolling back CreateSymlink without a backup simply removes
    /// the symlink (no file restoration).
    #[test]
    fn test_rollback_symlink_no_backup_removes_symlink() {
        let (_dir, base) = setup();
        let target = base.join("repo_file.txt");
        let link = base.join("link");

        std::fs::write(&target, "repo content").unwrap();

        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
            backup_path: None, // No backup recorded
            backup_exists: false,
        };

        // Execute: creates symlink
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(crate::symlink::is_symlink(&link));

        // Rollback: should just remove the symlink
        let rollback = action.rollback().unwrap();
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        // Symlink removed, no file at link location
        assert!(!crate::symlink::is_symlink(&link));
        assert!(!link.exists());
    }

    /// Test that rolling back CreateSymlink when backup path is provided
    /// but the backup file is missing falls back to symlink removal only.
    #[test]
    fn test_rollback_symlink_backup_missing_falls_back_to_removal() {
        let (_dir, base) = setup();
        let target = base.join("repo_file.txt");
        let link = base.join("link");
        let backup = base.join("backups/missing.txt"); // doesn't exist

        std::fs::write(&target, "repo content").unwrap();

        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
            backup_path: Some(backup), // Backup path recorded but file missing
            backup_exists: false,      // backup file does not exist
        };

        // Execute: creates symlink
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(crate::symlink::is_symlink(&link));

        // Rollback: backup doesn't exist, should fall back to RemoveSymlink
        let rollback = action.rollback().unwrap();
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        // Symlink removed, no file at link location
        assert!(!crate::symlink::is_symlink(&link));
        assert!(!link.exists());
    }

    /// Test the TOCTOU fix: when backup_exists is true at construction time
    /// but the backup file is deleted before rollback, the stored flag ensures
    /// the rollback still attempts to restore from backup (and fails gracefully)
    /// rather than silently removing the symlink without restoring content.
    ///
    /// This simulates the scenario where a concurrent `dotty clean` removes
    /// the backup between execution and rollback.
    #[test]
    fn test_rollback_symlink_backup_deleted_between_execution_and_rollback() {
        let (_dir, base) = setup();
        let target = base.join("repo_file.txt");
        let link = base.join("link");
        let backup = base.join("backups/original.txt");

        // Create the repo file (symlink target)
        std::fs::write(&target, "repo content").unwrap();
        // Create the backup (original file content saved before symlink)
        std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
        std::fs::write(&backup, "original content").unwrap();

        // Create action with backup_exists=true (backup existed at construction time)
        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
            backup_path: Some(backup.clone()),
            backup_exists: true,
        };

        // Execute: creates symlink at link → target
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(crate::symlink::is_symlink(&link));

        // Simulate TOCTOU: delete the backup between execution and rollback
        std::fs::remove_file(&backup).unwrap();
        assert!(!backup.exists());

        // Rollback: should still use RestoreBackup (not RemoveSymlink)
        // because backup_exists was true at construction time.
        // The RestoreBackup will fail because the source doesn't exist,
        // but it won't silently lose the symlink.
        let rollback = action.rollback().unwrap();
        match &rollback {
            Action::RestoreBackup { source, dest } => {
                assert_eq!(source, &backup);
                assert_eq!(dest, &link);
            }
            other => panic!(
                "expected RestoreBackup (TOCTOU fix: stored flag used), got {:?}",
                other
            ),
        }

        // Rollback will fail because backup was deleted, but the action type
        // is correct — it attempted to restore rather than silently removing.
        let result = action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        );
        assert!(result.is_err());

        // RestoreBackup removes the symlink first, then tries to copy the backup.
        // Since the backup doesn't exist, copy fails and the symlink is gone.
        // The key point is that the rollback *attempted* to restore (RestoreBackup)
        // rather than silently removing (RemoveSymlink). If the backup had existed,
        // the original content would have been restored.
        assert!(!crate::symlink::is_symlink(&link));
    }

    /// Test RestoreBackup action: copies backup file to destination,
    /// removing any existing symlink first.
    #[test]
    fn test_restore_backup_action() {
        let (_dir, base) = setup();
        let source = base.join("backups/original.txt");
        let dest = base.join("restored.txt");

        // Create the backup file
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "original content").unwrap();

        // Create a symlink at dest (simulating the state before rollback)
        let dummy_target = base.join("dummy");
        std::fs::write(&dummy_target, "dummy").unwrap();
        crate::symlink::create_symlink(&dummy_target, &dest).unwrap();
        assert!(crate::symlink::is_symlink(&dest));

        // Execute RestoreBackup
        let action = Action::RestoreBackup {
            source: source.clone(),
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();

        // Symlink removed, backup content restored
        assert!(!crate::symlink::is_symlink(&dest));
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original content");
    }

    /// Test RestoreBackup rollback: removes the restored file.
    #[test]
    fn test_rollback_restore_backup() {
        let (_dir, base) = setup();
        let source = base.join("backups/original.txt");
        let dest = base.join("restored.txt");

        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "original content").unwrap();

        let action = Action::RestoreBackup {
            source,
            dest: dest.clone(),
        };
        action_execute(
            &action,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(dest.exists());

        // Rollback of RestoreBackup removes the restored file
        let rollback = action.rollback().unwrap();
        action_execute(
            &rollback,
            &mut RepoState::new_for_git(dummy_repo_path(), dummy_repo_path()),
        )
        .unwrap();
        assert!(!dest.exists());
    }

    /// Test that CreateSymlink with backup_path roundtrips through
    /// pending plan serialization correctly.
    #[test]
    fn test_pending_plan_roundtrip_symlink_with_backup() {
        let (_dir, base) = setup();
        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateSymlink {
            target: base.join("repo_file"),
            link: base.join("link"),
            backup_path: Some(base.join("backups/original.txt")),
            backup_exists: true,
        });

        save_pending_plan(&plan, &state).unwrap();
        let loaded = load_pending_plan(&state).unwrap().unwrap();

        assert_eq!(loaded.actions.len(), 1);
        match &loaded.actions[0] {
            Action::CreateSymlink { backup_path, .. } => {
                assert!(backup_path.is_some());
                assert!(
                    backup_path
                        .as_ref()
                        .unwrap()
                        .ends_with("backups/original.txt")
                );
            }
            other => panic!("expected CreateSymlink, got {:?}", other),
        }
    }
}
