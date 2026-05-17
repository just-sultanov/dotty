use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;
use crate::convention::{
    expand_tilde, find_managed_repo_files, repo_to_target, walk_dir, write_config,
};
use crate::git;
use crate::plan::{self, Action, Plan};
use crate::prompt::prompt_confirm;
use crate::repo_state::RepoState;
use crate::symlink::is_symlink;

/// Run the `remove` command.
pub fn run(
    path: String,
    machine: Option<String>,
    commit: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let repo = RepoState::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    repo.require_git().map_err(|e| anyhow::anyhow!("{e}"))?;

    let repo_path = &repo.repo_path;
    let state_path = &repo.state_path;

    // Expand ~ in the input path
    let target_path = expand_tilde(&path)?;

    // Collect all files to remove (recursively for directories)
    let target_files = collect_target_files(&target_path)?;
    if target_files.is_empty() {
        anyhow::bail!("No files found at path: {}", target_path.display());
    }

    // Get tracked files from repo
    let tracked_files = git::git_ls_files(repo_path)?;

    // For each target file, find corresponding repo files
    let mut managed_pairs: Vec<(PathBuf, String)> = Vec::new();

    for target_file in &target_files {
        let repo_files = find_managed_repo_files(target_file, &tracked_files, machine.as_deref());

        if repo_files.is_empty() {
            // Check if this target file is covered by a directory prefix match
            // e.g., removing ~/.config/nvim/ should find base/home/.config/nvim/init.lua
            continue;
        }

        for repo_rel in repo_files {
            managed_pairs.push((target_file.clone(), repo_rel));
        }
    }

    // Also check for files under the target path (for directory removal)
    if target_path.is_dir() || target_path.to_string_lossy().ends_with('/') {
        for tracked in &tracked_files {
            let repo_path_buf = PathBuf::from(tracked);
            if let Ok(target) = repo_to_target(&repo_path_buf)
                && target.starts_with(&target_path)
            {
                // Check if already added
                let already = managed_pairs.iter().any(|(_, r)| r == tracked);
                if !already
                    && (machine.is_none()
                        || machine.as_ref().is_some_and(|m| {
                            let prefix = format!("{}/", m);
                            tracked.starts_with(&prefix)
                        }))
                {
                    managed_pairs.push((target.clone(), tracked.clone()));
                }
            }
        }
    }

    if managed_pairs.is_empty() {
        anyhow::bail!("Path not managed by dotty: {}", target_path.display());
    }

    // Deduplicate by repo path
    let mut seen = HashSet::new();
    managed_pairs.retain(|(_, repo_rel)| seen.insert(repo_rel.clone()));

    // Read current config (to update managed map)
    let config = repo.config;

    // Resolve user prompts for files that need override confirmation
    let skipped = resolve_remove_skipped(&managed_pairs, repo_path)?;

    // Build the plan (pure function — no side effects)
    let input = RemovePlanInput {
        repo_path: repo_path.clone(),
        managed_pairs,
        skipped,
        commit: commit.clone(),
    };
    let output = build_remove_plan(&input, &config)?;

    // Execute plan
    plan::execute_plan(&output.plan, dry_run, state_path)?;

    // Write updated config only after successful plan execution
    if !dry_run && !output.plan.is_empty() {
        write_config(state_path, &output.config)?;
    }

    // Print summary
    let total = input.managed_pairs.len();
    if dry_run {
        println!(
            "[dry-run] {} file(s) would be removed from management",
            total
        );
        println!("[dry-run] no changes made");
    } else if commit.is_some() {
        println!("Removed {} file(s) from management.", total);
    } else {
        println!(
            "Removed {} file(s) from management. Run `git rm` + `git commit` to finalize.",
            total
        );
    }

    Ok(())
}

/// Collect all target files under the given path.
fn collect_target_files(target_path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if target_path.is_file() || is_symlink(target_path) {
        files.push(target_path.to_path_buf());
    } else if target_path.is_dir() {
        walk_dir(target_path, &mut files)?;
    } else {
        // Path doesn't exist yet — treat it as a single target for lookup
        files.push(target_path.to_path_buf());
    }

    Ok(files)
}

// ---------------------------------------------------------------------------
// Plan building (pure — no I/O)
// ---------------------------------------------------------------------------

/// Input data for building a `remove` plan.
pub(crate) struct RemovePlanInput {
    pub repo_path: PathBuf,
    pub managed_pairs: Vec<(PathBuf, String)>,
    pub skipped: HashSet<String>,
    pub commit: Option<String>,
}

/// Output of `build_remove_plan`.
pub(crate) struct RemovePlanOutput {
    pub plan: Plan,
    pub config: Config,
}

/// Build a plan for removing files from the dotty repository.
///
/// This is a pure function: it takes resolved input data (managed pairs,
/// skipped files from user prompts) and returns a `Plan` with actions
/// and an updated `Config`. No filesystem or git operations are performed.
pub(crate) fn build_remove_plan(
    input: &RemovePlanInput,
    config: &Config,
) -> Result<RemovePlanOutput> {
    let mut plan = Plan::new(&input.repo_path);
    let mut config = config.clone();

    // Collect repo-relative paths for git staging
    let mut git_rm_paths: Vec<PathBuf> = Vec::new();

    // Phase 1: Remove symlinks at target locations.
    for (target_file, _repo_rel) in &input.managed_pairs {
        if is_symlink(target_file) {
            plan.add(Action::RemoveSymlink {
                path: target_file.clone(),
            });
        }
    }

    // Phase 2: Copy files from repo back to target (restore as regular files).
    for (target_file, repo_rel) in &input.managed_pairs {
        if input.skipped.contains(repo_rel) {
            continue;
        }

        let repo_file = input.repo_path.join(repo_rel);

        if repo_file.exists() {
            plan.add(Action::CopyFile {
                source: repo_file.clone(),
                dest: target_file.clone(),
            });
        }
    }

    // Phase 3: Remove files from repo and update config.
    for (_target_file, repo_rel) in &input.managed_pairs {
        if input.skipped.contains(repo_rel) {
            continue;
        }

        let repo_file = input.repo_path.join(repo_rel);

        plan.add(Action::RemoveFile {
            path: repo_file.clone(),
        });

        config.managed.shift_remove(repo_rel);

        git_rm_paths.push(PathBuf::from(repo_rel));
    }

    // Stage deletions in git
    if !git_rm_paths.is_empty() {
        plan.add(Action::GitAdd {
            paths: git_rm_paths,
        });
    }

    // Git commit (if --commit specified)
    if let Some(ref msg) = input.commit {
        plan.add(Action::GitCommit {
            message: msg.clone(),
        });
    }

    Ok(RemovePlanOutput { plan, config })
}

/// Resolve which files the user wants to skip during removal.
///
/// For each managed pair where the repo file exists and the target already
/// exists as a regular file (not a symlink), ask the user for override
/// confirmation. Returns the set of repo-relative paths the user declined.
fn resolve_remove_skipped(
    managed_pairs: &[(PathBuf, String)],
    repo_path: &Path,
) -> Result<HashSet<String>> {
    let mut skipped = HashSet::new();

    for (target_file, repo_rel) in managed_pairs {
        let repo_file = repo_path.join(repo_rel);

        if repo_file.exists() && target_file.exists() && !is_symlink(target_file) {
            let ok = prompt_confirm(&format!(
                "Override existing file at {}?",
                target_file.display()
            ))?;
            if !ok {
                skipped.insert(repo_rel.clone());
            }
        }
    }

    Ok(skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a unique temporary directory that is automatically cleaned up on drop.
    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_collect_target_files_single() {
        let dir = test_dir();
        let path = dir.path().to_path_buf();
        let file = path.join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let files = collect_target_files(&file).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], file);
    }

    #[test]
    fn test_collect_target_files_directory() {
        let dir = test_dir();
        let path = dir.path().to_path_buf();
        std::fs::create_dir_all(path.join("sub")).unwrap();
        std::fs::write(path.join("a.txt"), "a").unwrap();
        std::fs::write(path.join("sub").join("b.txt"), "b").unwrap();

        let files = collect_target_files(&path).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_collect_target_files_nonexistent() {
        let dir = test_dir();
        let path = dir.path().join("nonexistent.txt");
        let files = collect_target_files(&path).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], path);
    }

    // -- build_remove_plan tests --

    #[test]
    fn test_build_remove_plan_basic() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        std::fs::write(&target, "content").unwrap();

        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // CopyFile + RemoveFile + GitAdd = 3 actions (no symlink to remove)
        assert_eq!(output.plan.actions.len(), 3);
        assert!(!output.config.managed.contains_key("base/home/.vimrc"));
    }

    #[test]
    fn test_build_remove_plan_with_symlink() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();
        crate::symlink::create_symlink(&repo_file, &target).unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // RemoveSymlink + CopyFile + RemoveFile + GitAdd = 4 actions
        assert_eq!(output.plan.actions.len(), 4);
    }

    #[test]
    fn test_build_remove_plan_with_skipped() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        std::fs::write(&target, "content").unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let mut skipped = HashSet::new();
        skipped.insert("base/home/.vimrc".to_string());

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped,
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // Skipped: no actions, managed map unchanged
        assert!(output.plan.is_empty());
        assert!(output.config.managed.contains_key("base/home/.vimrc"));
    }

    #[test]
    fn test_build_remove_plan_with_git_commit() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        std::fs::write(&target, "content").unwrap();

        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: Some("remove vimrc".to_string()),
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // CopyFile + RemoveFile + GitAdd + GitCommit = 4
        assert_eq!(output.plan.actions.len(), 4);

        match &output.plan.actions.last().unwrap() {
            Action::GitCommit { message } => assert_eq!(message, "remove vimrc"),
            other => panic!("expected GitCommit, got: {other:?}"),
        }
    }
}
