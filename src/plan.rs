use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, trace, warn};

/// Maximum number of paths to show in `GitAdd` action display.
const GIT_ADD_MAX_SHOWN: usize = 3;

use crate::error::DottyError;
use crate::git;
use crate::symlink::{self, is_symlink};

/// A single atomic operation within a plan.
///
/// Each action can be executed and, if needed, rolled back.
#[derive(Debug, Clone)]
pub(crate) enum Action {
    CreateDir { path: PathBuf },
    Backup { source: PathBuf, dest: PathBuf },
    CopyFile { source: PathBuf, dest: PathBuf },
    CreateSymlink { target: PathBuf, link: PathBuf },
    RemoveFile { path: PathBuf },
    RemoveSymlink { path: PathBuf },
    GitAdd { paths: Vec<PathBuf> },
    GitCommit { message: String },
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::CreateDir { path } => write!(f, "create dir    {}", path.display()),
            Action::Backup { source, dest } => {
                write!(f, "backup        {} → {}", source.display(), dest.display())
            }
            Action::CopyFile { source, dest } => {
                write!(f, "copy file     {} → {}", source.display(), dest.display())
            }
            Action::CreateSymlink { target, link } => {
                write!(f, "create link   {} → {}", link.display(), target.display())
            }
            Action::RemoveFile { path } => write!(f, "remove file   {}", path.display()),
            Action::RemoveSymlink { path } => write!(f, "remove link   {}", path.display()),
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
    /// Perform the filesystem or git mutation described by this action.
    ///
    /// `repo_path` is used as the working directory for git operations.
    pub fn execute(&self, repo_path: &Path) -> Result<(), DottyError> {
        match self {
            Action::CreateDir { path } => {
                fs::create_dir_all(path).map_err(|e| io_error_with_path(e, path))?;
            }
            Action::Backup { source, dest } => {
                let parent = dest.parent().ok_or_else(|| DottyError::PathResolution {
                    path: dest.to_path_buf(),
                    reason: format!("cannot determine parent of backup path: {}", dest.display()),
                })?;
                fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
                // copy_file_dereference already returns DottyError
                copy_file_dereference(source, dest)?;
                // Verify backup integrity: check existence and size match
                verify_backup_integrity(source, dest)?;
            }
            Action::CopyFile { source, dest } => {
                let parent = dest.parent();
                if let Some(p) = parent {
                    fs::create_dir_all(p).map_err(|e| io_error_with_path(e, p))?;
                }
                // copy_file_dereference already returns DottyError
                copy_file_dereference(source, dest)?;
            }
            Action::CreateSymlink { target, link } => {
                let parent = link.parent();
                if let Some(p) = parent {
                    fs::create_dir_all(p).map_err(|e| io_error_with_path(e, p))?;
                }
                // Detect circular symlinks before creating
                if symlink::would_be_circular(target, link) {
                    return Err(DottyError::CircularSymlink { path: link.clone() });
                }
                // Use symlink_metadata to detect both existing files and broken symlinks.
                // `link.exists()` returns false for broken symlinks, so we check metadata directly.
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
    pub fn rollback(&self) -> Option<Action> {
        match self {
            Action::CreateDir { path } => Some(Action::RemoveFile { path: path.clone() }),
            Action::Backup { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
            Action::CopyFile { dest, .. } => Some(Action::RemoveFile { path: dest.clone() }),
            Action::CreateSymlink { link, .. } => {
                Some(Action::RemoveSymlink { path: link.clone() })
            }
            Action::RemoveFile { path: _ } => None,
            Action::RemoveSymlink { path: _, .. } => None,
            Action::GitAdd { .. } => None,
            Action::GitCommit { .. } => None,
        }
    }
}

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

/// Execute all actions in the plan.
///
/// If `dry_run` is true, print each action but perform no mutations.
/// If any action fails, roll back all previously completed actions in
/// reverse order.
///
/// `state_path` is used to save a pending plan before execution for
/// crash recovery. The pending plan is cleared on success.
pub(crate) fn execute_plan(
    plan: &Plan,
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
    save_pending_plan(plan, state_path)?;

    let mut completed: Vec<usize> = Vec::new();
    let check = crate::symbols::check();

    for (i, action) in plan.actions.iter().enumerate() {
        trace!("executing action {}: {}", i + 1, action);
        print!("  {}. {} ... ", i + 1, action);
        match action.execute(&plan.repo_path) {
            Ok(()) => {
                println!("{check}");
                completed.push(i);
            }
            Err(e) => {
                warn!("action {} failed: {}", i + 1, e);
                println!("FAILED: {e}");
                rollback_completed(plan, &completed)?;
                return Err(e);
            }
        }
    }

    // All actions succeeded — clear pending plan
    clear_pending_plan(state_path)?;

    Ok(())
}

/// Roll back completed actions in reverse order.
///
/// Handles git actions specially (reset --soft for commits, reset HEAD for adds)
/// because their rollback is not expressible as a simple inverse Action.
fn rollback_completed(plan: &Plan, completed_indices: &[usize]) -> Result<(), DottyError> {
    debug!("rolling back {} completed actions", completed_indices.len());
    let actions = &plan.actions;
    let repo_path = &plan.repo_path;

    let mut has_git_add = false;
    let mut git_add_paths: Vec<PathBuf> = Vec::new();

    let mut indices: Vec<usize> = completed_indices.to_vec();
    indices.sort_unstable();
    indices.reverse();

    for &idx in &indices {
        let action = &actions[idx];

        match action {
            Action::GitCommit { .. } => {
                println!("  rollback: git reset --soft HEAD~1");
                git::git_reset_soft_head(repo_path)?;
                continue;
            }
            Action::GitAdd { paths } => {
                has_git_add = true;
                git_add_paths.extend(paths.clone());
                continue;
            }
            _ => {}
        }

        if let Some(rollback_action) = action.rollback() {
            println!("  rollback: {}", rollback_action);
            rollback_action.execute(repo_path)?;
        }
    }

    if has_git_add {
        let path_strs: Vec<&str> = git_add_paths.iter().filter_map(|p| p.to_str()).collect();
        if !path_strs.is_empty() {
            println!("  rollback: git reset HEAD {}", path_strs.join(" "));
            git::git_reset(repo_path, &path_strs)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Pending plan recovery
// ---------------------------------------------------------------------------

/// Filename for the pending plan file inside the state directory.
const PENDING_PLAN_FILE: &str = "pending_plan.json";

/// Serializable action (uses `String` for paths).
#[derive(Debug, Clone, Serialize, Deserialize)]
enum SerializableAction {
    CreateDir { path: String },
    Backup { source: String, dest: String },
    CopyFile { source: String, dest: String },
    CreateSymlink { target: String, link: String },
    RemoveFile { path: String },
    RemoveSymlink { path: String },
    GitAdd { paths: Vec<String> },
    GitCommit { message: String },
}

impl From<&Action> for SerializableAction {
    fn from(action: &Action) -> Self {
        match action {
            Action::CreateDir { path } => SerializableAction::CreateDir {
                path: path.to_string_lossy().to_string(),
            },
            Action::Backup { source, dest } => SerializableAction::Backup {
                source: source.to_string_lossy().to_string(),
                dest: dest.to_string_lossy().to_string(),
            },
            Action::CopyFile { source, dest } => SerializableAction::CopyFile {
                source: source.to_string_lossy().to_string(),
                dest: dest.to_string_lossy().to_string(),
            },
            Action::CreateSymlink { target, link } => SerializableAction::CreateSymlink {
                target: target.to_string_lossy().to_string(),
                link: link.to_string_lossy().to_string(),
            },
            Action::RemoveFile { path } => SerializableAction::RemoveFile {
                path: path.to_string_lossy().to_string(),
            },
            Action::RemoveSymlink { path } => SerializableAction::RemoveSymlink {
                path: path.to_string_lossy().to_string(),
            },
            Action::GitAdd { paths } => SerializableAction::GitAdd {
                paths: paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect(),
            },
            Action::GitCommit { message } => SerializableAction::GitCommit {
                message: message.clone(),
            },
        }
    }
}

impl SerializableAction {
    /// Convert back to an `Action` with `PathBuf` fields.
    fn into_action(self) -> Action {
        match self {
            SerializableAction::CreateDir { path } => Action::CreateDir {
                path: PathBuf::from(path),
            },
            SerializableAction::Backup { source, dest } => Action::Backup {
                source: PathBuf::from(source),
                dest: PathBuf::from(dest),
            },
            SerializableAction::CopyFile { source, dest } => Action::CopyFile {
                source: PathBuf::from(source),
                dest: PathBuf::from(dest),
            },
            SerializableAction::CreateSymlink { target, link } => Action::CreateSymlink {
                target: PathBuf::from(target),
                link: PathBuf::from(link),
            },
            SerializableAction::RemoveFile { path } => Action::RemoveFile {
                path: PathBuf::from(path),
            },
            SerializableAction::RemoveSymlink { path } => Action::RemoveSymlink {
                path: PathBuf::from(path),
            },
            SerializableAction::GitAdd { paths } => Action::GitAdd {
                paths: paths.into_iter().map(PathBuf::from).collect(),
            },
            SerializableAction::GitCommit { message } => Action::GitCommit { message },
        }
    }
}

/// A pending plan saved to disk for recovery after interrupted operations.
#[derive(Debug, Serialize, Deserialize)]
struct PendingPlan {
    /// Path to the dotty repository.
    repo_path: String,
    /// Actions that were planned but may not have completed.
    actions: Vec<SerializableAction>,
}

impl PendingPlan {
    /// Convert a `Plan` into a `PendingPlan` for serialization.
    fn from_plan(plan: &Plan) -> Self {
        Self {
            repo_path: plan.repo_path.to_string_lossy().to_string(),
            actions: plan.actions.iter().map(SerializableAction::from).collect(),
        }
    }

    /// Convert back to an executable `Plan`.
    fn to_plan(&self) -> Plan {
        Plan {
            repo_path: PathBuf::from(&self.repo_path),
            actions: self
                .actions
                .iter()
                .cloned()
                .map(|a| a.into_action())
                .collect(),
        }
    }
}

/// Path to the pending plan file inside the state directory.
fn pending_plan_path(state_path: &Path) -> PathBuf {
    state_path.join(PENDING_PLAN_FILE)
}

/// Save a plan to disk before execution.
///
/// If the process is killed (SIGKILL, crash) during execution, the pending
/// plan file remains and can be used for recovery on the next run.
pub(crate) fn save_pending_plan(plan: &Plan, state_path: &Path) -> Result<(), DottyError> {
    fs::create_dir_all(state_path)?;
    let pending = PendingPlan::from_plan(plan);
    let content = serde_json::to_string_pretty(&pending)?;
    fs::write(pending_plan_path(state_path), content)?;
    debug!(
        "saved pending plan to {}",
        pending_plan_path(state_path).display()
    );
    Ok(())
}

/// Load a pending plan from disk, if one exists.
///
/// Returns `Ok(None)` if no pending plan file exists.
pub(crate) fn load_pending_plan(state_path: &Path) -> Result<Option<Plan>, DottyError> {
    let path = pending_plan_path(state_path);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let pending: PendingPlan = serde_json::from_str(&content)?;
    debug!("loaded pending plan from {}", path.display());
    Ok(Some(pending.to_plan()))
}

/// Remove the pending plan file (called after successful execution).
pub(crate) fn clear_pending_plan(state_path: &Path) -> Result<(), DottyError> {
    let path = pending_plan_path(state_path);
    if path.exists() {
        fs::remove_file(&path)?;
        debug!("cleared pending plan at {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Copy a file, dereferencing symlinks (equivalent to `cp -L`).
fn copy_file_dereference(source: &Path, dest: &Path) -> Result<(), DottyError> {
    let content = fs::read(source)?;
    fs::write(dest, content)?;
    Ok(())
}

/// Verify that a backup file was created correctly.
///
/// Checks that the backup exists at the destination path and that its size
/// matches the source file. Returns an error if either check fails, preventing
/// the plan from proceeding to replace the original file with an unverified backup.
fn verify_backup_integrity(source: &Path, dest: &Path) -> Result<(), DottyError> {
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
///
/// If the error is `PermissionDenied`, returns `DottyError::PermissionDenied`
/// with a clear message. Otherwise, wraps the IO error as usual.
fn io_error_with_path(err: io::Error, path: &Path) -> DottyError {
    if err.kind() == io::ErrorKind::PermissionDenied {
        DottyError::PermissionDenied {
            path: path.to_path_buf(),
        }
    } else {
        DottyError::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("dotty_plan_test_{}_{}", std::process::id(), id))
    }

    fn setup() -> PathBuf {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn teardown(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn dummy_repo_path() -> PathBuf {
        PathBuf::from(".")
    }

    #[test]
    fn test_create_dir_action() {
        let base = setup();
        let path = base.join("new_dir/nested");

        let action = Action::CreateDir { path: path.clone() };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(path.is_dir());

        teardown(&base);
    }

    #[test]
    fn test_copy_file_action() {
        let base = setup();
        let src = base.join("source.txt");
        let dst = base.join("dest.txt");

        fs::write(&src, "hello world").unwrap();

        let action = Action::CopyFile {
            source: src.clone(),
            dest: dst.clone(),
        };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(dst.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "hello world");

        teardown(&base);
    }

    #[test]
    fn test_copy_file_creates_parent_dirs() {
        let base = setup();
        let src = base.join("source.txt");
        let dst = base.join("a/b/c/dest.txt");

        fs::write(&src, "data").unwrap();

        let action = Action::CopyFile {
            source: src,
            dest: dst.clone(),
        };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(dst.exists());

        teardown(&base);
    }

    #[test]
    fn test_backup_action() {
        let base = setup();
        let src = base.join("original.txt");
        let backup_dir = base.join("backups/2024-01-01T00-00-00");
        let dst = backup_dir.join("original.txt");

        fs::write(&src, "original content").unwrap();

        let action = Action::Backup {
            source: src,
            dest: dst.clone(),
        };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(dst.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "original content");

        teardown(&base);
    }

    #[test]
    fn test_remove_file_action() {
        let base = setup();
        let path = base.join("to_remove.txt");
        fs::write(&path, "delete me").unwrap();

        let action = Action::RemoveFile { path: path.clone() };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(!path.exists());

        teardown(&base);
    }

    #[test]
    fn test_remove_file_idempotent() {
        let base = setup();
        let path = base.join("does_not_exist.txt");

        let action = Action::RemoveFile { path };
        action.execute(&dummy_repo_path()).unwrap();

        teardown(&base);
    }

    #[test]
    fn test_create_symlink_action() {
        let base = setup();
        let target = base.join("real_file.txt");
        let link = base.join("link_to_file");

        fs::write(&target, "content").unwrap();

        let action = Action::CreateSymlink {
            target: target.clone(),
            link: link.clone(),
        };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(is_symlink(&link));
        assert_eq!(fs::read_link(&link).unwrap(), target);

        teardown(&base);
    }

    #[test]
    fn test_create_symlink_replaces_existing() {
        let base = setup();
        let target1 = base.join("file1.txt");
        let target2 = base.join("file2.txt");
        let link = base.join("link");

        fs::write(&target1, "one").unwrap();
        fs::write(&target2, "two").unwrap();

        Action::CreateSymlink {
            target: target1.clone(),
            link: link.clone(),
        }
        .execute(&dummy_repo_path())
        .unwrap();

        Action::CreateSymlink {
            target: target2.clone(),
            link: link.clone(),
        }
        .execute(&dummy_repo_path())
        .unwrap();

        assert!(is_symlink(&link));
        assert_eq!(fs::read_link(&link).unwrap(), target2);

        teardown(&base);
    }

    #[test]
    fn test_rollback_create_dir() {
        let base = setup();
        let path = base.join("rollback_dir");

        let action = Action::CreateDir { path: path.clone() };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(path.is_dir());

        let rollback = action.rollback().unwrap();
        rollback.execute(&dummy_repo_path()).unwrap();
        assert!(!path.exists());

        teardown(&base);
    }

    #[test]
    fn test_rollback_copy_file() {
        let base = setup();
        let src = base.join("src.txt");
        let dst = base.join("dst.txt");

        fs::write(&src, "data").unwrap();

        let action = Action::CopyFile {
            source: src,
            dest: dst.clone(),
        };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(dst.exists());

        let rollback = action.rollback().unwrap();
        rollback.execute(&dummy_repo_path()).unwrap();
        assert!(!dst.exists());

        teardown(&base);
    }

    #[test]
    fn test_rollback_symlink() {
        let base = setup();
        let target = base.join("target.txt");
        let link = base.join("link");

        fs::write(&target, "content").unwrap();

        let action = Action::CreateSymlink {
            target,
            link: link.clone(),
        };
        action.execute(&dummy_repo_path()).unwrap();
        assert!(is_symlink(&link));

        let rollback = action.rollback().unwrap();
        rollback.execute(&dummy_repo_path()).unwrap();
        assert!(!is_symlink(&link));
        assert!(!link.exists());

        teardown(&base);
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
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();
        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("should_not_exist"),
        });

        execute_plan(&plan, true, &state).unwrap();
        assert!(!base.join("should_not_exist").exists());

        teardown(&base);
    }

    #[test]
    fn test_execute_plan_empty() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();
        let plan = Plan::new(&base);
        execute_plan(&plan, false, &state).unwrap();
        teardown(&base);
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
    fn test_copy_file_dereferences_symlink() {
        let base = setup();
        let real = base.join("real.txt");
        let sym = base.join("sym.txt");
        let dst = base.join("copied.txt");

        fs::write(&real, "real content").unwrap();
        crate::symlink::create_symlink(&real, &sym).unwrap();

        copy_file_dereference(&sym, &dst).unwrap();
        assert!(!is_symlink(&dst));
        assert_eq!(fs::read_to_string(&dst).unwrap(), "real content");

        teardown(&base);
    }

    #[test]
    fn test_backup_verification_success() {
        let base = setup();
        let src = base.join("source.txt");
        let dst = base.join("backup.txt");

        fs::write(&src, "original content").unwrap();
        fs::write(&dst, "original content").unwrap();

        verify_backup_integrity(&src, &dst).unwrap();

        teardown(&base);
    }

    #[test]
    fn test_backup_verification_size_mismatch() {
        let base = setup();
        let src = base.join("source.txt");
        let dst = base.join("backup.txt");

        fs::write(&src, "original content").unwrap();
        fs::write(&dst, "short").unwrap();

        let result = verify_backup_integrity(&src, &dst);
        assert!(result.is_err());
        match result.unwrap_err() {
            DottyError::BackupVerification { path, detail } => {
                assert_eq!(path, dst);
                assert!(detail.contains("size mismatch"));
            }
            other => panic!("expected BackupVerification error, got: {other}"),
        }

        teardown(&base);
    }

    #[test]
    fn test_backup_verification_missing_backup() {
        let base = setup();
        let src = base.join("source.txt");
        let dst = base.join("backup_missing.txt");

        fs::write(&src, "content").unwrap();
        // dst does not exist

        let result = verify_backup_integrity(&src, &dst);
        assert!(result.is_err());
        match result.unwrap_err() {
            DottyError::BackupVerification { path, detail } => {
                assert_eq!(path, dst);
                assert!(detail.contains("does not exist") || detail.contains("not readable"));
            }
            other => panic!("expected BackupVerification error, got: {other}"),
        }

        teardown(&base);
    }

    #[test]
    fn test_backup_verification_empty_files() {
        let base = setup();
        let src = base.join("empty.txt");
        let dst = base.join("empty_backup.txt");

        fs::write(&src, "").unwrap();
        fs::write(&dst, "").unwrap();

        // Two empty files should pass verification (both 0 bytes)
        verify_backup_integrity(&src, &dst).unwrap();

        teardown(&base);
    }

    #[test]
    fn test_backup_action_with_verification() {
        let base = setup();
        let src = base.join("original.txt");
        let backup_dir = base.join("backups/2024-01-01T00-00-00");
        let dst = backup_dir.join("original.txt");

        fs::write(&src, "original content").unwrap();

        let action = Action::Backup {
            source: src,
            dest: dst.clone(),
        };
        // Should succeed: copy + verify
        action.execute(&dummy_repo_path()).unwrap();
        assert!(dst.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "original content");

        teardown(&base);
    }

    // -- pending plan tests --

    #[test]
    fn test_save_and_load_pending_plan() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();

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

        teardown(&base);
    }

    #[test]
    fn test_load_pending_plan_missing() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();

        let loaded = load_pending_plan(&state).unwrap();
        assert!(loaded.is_none());

        teardown(&base);
    }

    #[test]
    fn test_clear_pending_plan() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("dir"),
        });

        save_pending_plan(&plan, &state).unwrap();
        assert!(state.join("pending_plan.json").exists());

        clear_pending_plan(&state).unwrap();
        assert!(!state.join("pending_plan.json").exists());

        teardown(&base);
    }

    #[test]
    fn test_clear_pending_plan_idempotent() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();

        // Clearing when no file exists should not error
        clear_pending_plan(&state).unwrap();
        clear_pending_plan(&state).unwrap();

        teardown(&base);
    }

    #[test]
    fn test_pending_plan_roundtrip_all_action_types() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();

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

        teardown(&base);
    }

    #[test]
    fn test_execute_plan_saves_and_clears_pending() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("test_dir"),
        });

        // Before execution, no pending plan
        assert!(!state.join("pending_plan.json").exists());

        // Execute plan
        execute_plan(&plan, false, &state).unwrap();

        // After successful execution, pending plan is cleared
        assert!(!state.join("pending_plan.json").exists());
        assert!(base.join("test_dir").is_dir());

        teardown(&base);
    }

    #[test]
    fn test_execute_plan_dry_run_does_not_save_pending() {
        let base = setup();
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();

        let mut plan = Plan::new(&base);
        plan.add(Action::CreateDir {
            path: base.join("should_not_exist"),
        });

        execute_plan(&plan, true, &state).unwrap();

        // Dry run should not create pending plan file
        assert!(!state.join("pending_plan.json").exists());
        assert!(!base.join("should_not_exist").exists());

        teardown(&base);
    }
}
