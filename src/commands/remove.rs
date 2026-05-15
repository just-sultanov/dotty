use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::convention::{
    expand_tilde, find_managed_repo_files, read_config, repo_to_target, resolve_repo_path,
    resolve_state_path, walk_dir, write_config,
};
use crate::git;
use crate::plan::{self, Action, Plan};
use crate::prompt::prompt_confirm;
use crate::symlink::is_symlink;

/// Run the `remove` command.
pub fn run(
    path: String,
    machine: Option<String>,
    commit: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let repo_path = resolve_repo_path()?;
    let state_path = resolve_state_path()?;

    // Ensure repo exists
    if !repo_path.join(".git").exists() {
        anyhow::bail!(
            "No dotty repository found at {}. Run `dotty init` first.",
            repo_path.display()
        );
    }

    // Expand ~ in the input path
    let target_path = expand_tilde(&path)?;

    // Collect all files to remove (recursively for directories)
    let target_files = collect_target_files(&target_path)?;
    if target_files.is_empty() {
        anyhow::bail!("No files found at path: {}", target_path.display());
    }

    // Get tracked files from repo
    let tracked_files = git::git_ls_files(&repo_path)?;

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

    // Build plan
    let mut plan = Plan::new(&repo_path);

    // Read current config (to update managed map)
    let mut config = read_config(&state_path)?;

    // Collect repo-relative paths for git staging
    let mut git_rm_paths: Vec<PathBuf> = Vec::new();

    // Track files the user chose to skip (e.g., declined override prompt)
    let mut skipped: HashSet<String> = HashSet::new();

    // Phase 1: Remove symlinks at target locations.
    // Done first so that Phase 2's CopyFile writes a new regular file instead of
    // following the symlink (`fs::write` follows symlinks). If the later copy fails,
    // the repo file is still intact and the user can re-apply.
    for (target_file, _repo_rel) in &managed_pairs {
        if is_symlink(target_file) {
            plan.add(Action::RemoveSymlink {
                path: target_file.clone(),
            });
        }
    }

    // Phase 2: Copy files from repo back to target (restore as regular files).
    // Symlinks are already gone, so `fs::write` creates a new regular file.
    for (target_file, repo_rel) in &managed_pairs {
        let repo_file = repo_path.join(repo_rel);

        if repo_file.exists() {
            // Check if target already exists as regular file — ask for override
            if target_file.exists() && !is_symlink(target_file) {
                let ok = prompt_confirm(&format!(
                    "Override existing file at {}?",
                    target_file.display()
                ))?;
                if !ok {
                    skipped.insert(repo_rel.clone());
                    continue;
                }
            }

            plan.add(Action::CopyFile {
                source: repo_file.clone(),
                dest: target_file.clone(),
            });
        }
    }

    // Phase 3: Remove files from repo and update config.
    // Done last so that if this fails, the target already has a valid regular file
    // (restored in phase 2) and the repository simply retains the extra file.
    for (_target_file, repo_rel) in &managed_pairs {
        if skipped.contains(repo_rel) {
            continue;
        }

        let repo_file = repo_path.join(repo_rel);

        // Remove file from repo
        plan.add(Action::RemoveFile {
            path: repo_file.clone(),
        });

        // Remove from managed map
        config.managed.shift_remove(repo_rel);

        // Track repo-relative path for git staging
        git_rm_paths.push(PathBuf::from(repo_rel));
    }

    // Stage deletions in git (git add stages removals of tracked files)
    if !git_rm_paths.is_empty() {
        plan.add(Action::GitAdd {
            paths: git_rm_paths.clone(),
        });
    }

    // Git commit (if --commit specified)
    if let Some(ref msg) = commit {
        plan.add(Action::GitCommit {
            message: msg.clone(),
        });
    }

    // Execute plan
    plan::execute_plan(&plan, dry_run)?;

    // Write updated config only after successful plan execution
    if !dry_run && !plan.is_empty() {
        write_config(&state_path, &config)?;
    }

    // Print summary
    if dry_run {
        println!(
            "[dry-run] {} file(s) would be removed from management",
            managed_pairs.len()
        );
        println!("[dry-run] no changes made");
    } else if commit.is_some() {
        println!("Removed {} file(s) from management.", managed_pairs.len());
    } else {
        println!(
            "Removed {} file(s) from management. Run `git rm` + `git commit` to finalize.",
            managed_pairs.len()
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn test_collect_target_files_single() {
        let dir = std::env::temp_dir().join(format!("dotty_remove_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        fs::write(&file, "content").unwrap();

        let files = collect_target_files(&file).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], file);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_collect_target_files_directory() {
        let dir =
            std::env::temp_dir().join(format!("dotty_remove_test_dir_{}", std::process::id()));
        fs::create_dir_all(&dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("sub").join("b.txt"), "b").unwrap();

        let files = collect_target_files(&dir).unwrap();
        assert_eq!(files.len(), 2);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_collect_target_files_nonexistent() {
        let path = PathBuf::from(
            "/tmp/dotty_nonexistent_{}.txt".to_string() + &std::process::id().to_string(),
        );
        let files = collect_target_files(&path).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], path);
    }
}
