use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{debug, trace, warn};

/// Maximum number of paths to show in `GitAdd` action display.
const GIT_ADD_MAX_SHOWN: usize = 3;

use crate::error::DottyError;
use crate::git;
use crate::symlink::{self, is_symlink};

#[allow(dead_code)]
/// A single atomic operation within a plan.
///
/// Each action can be executed and, if needed, rolled back.
#[derive(Debug, Clone)]
pub enum Action {
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
                let parent = dest.parent().ok_or_else(|| {
                    DottyError::Path(format!(
                        "cannot determine parent of backup path: {}",
                        dest.display()
                    ))
                })?;
                fs::create_dir_all(parent).map_err(|e| io_error_with_path(e, parent))?;
                // copy_file_dereference already returns DottyError
                copy_file_dereference(source, dest)?;
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

#[allow(dead_code)]
/// A plan is a sequence of actions to be executed together.
///
/// Built in a pure phase (no side effects), then executed with automatic
/// rollback on failure.
#[derive(Debug)]
pub struct Plan {
    pub repo_path: PathBuf,
    pub branch: String,
    pub command: String,
    pub actions: Vec<Action>,
}

#[allow(dead_code)]
impl Plan {
    /// Create a new empty plan.
    pub fn new(command: &str, repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            branch: String::new(),
            command: command.to_string(),
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

#[allow(dead_code)]
/// Execute all actions in the plan.
///
/// If `dry_run` is true, print each action but perform no mutations.
/// If any action fails, roll back all previously completed actions in
/// reverse order.
pub fn execute_plan(plan: &Plan, dry_run: bool) -> Result<(), DottyError> {
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

    Ok(())
}

#[allow(dead_code)]
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

#[allow(dead_code)]
/// Copy a file, dereferencing symlinks (equivalent to `cp -L`).
fn copy_file_dereference(source: &Path, dest: &Path) -> Result<(), DottyError> {
    let content = fs::read(source)?;
    fs::write(dest, content)?;
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
        let plan = Plan::new("test", &dummy_repo_path());
        assert!(plan.is_empty());
    }

    #[test]
    fn test_plan_add_actions() {
        let mut plan = Plan::new("test", &dummy_repo_path());
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
        let mut plan = Plan::new("test", &base);
        plan.add(Action::CreateDir {
            path: base.join("should_not_exist"),
        });

        execute_plan(&plan, true).unwrap();
        assert!(!base.join("should_not_exist").exists());

        teardown(&base);
    }

    #[test]
    fn test_execute_plan_empty() {
        let plan = Plan::new("test", &dummy_repo_path());
        execute_plan(&plan, false).unwrap();
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
}
