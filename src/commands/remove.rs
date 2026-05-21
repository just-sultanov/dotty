use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::DottyError;

use crate::config::Config;
use crate::config::write_config;
use crate::convention::find_managed_repo_files;
use crate::fs_utils::walk_dir;
use crate::git;
use crate::paths::{expand_tilde, repo_to_target};
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
) -> Result<(), DottyError> {
    let repo = RepoState::new()?;
    repo.require_git()?;

    let repo_path = &repo.repo_path;
    let state_path = &repo.state_path;

    // Expand ~ in the input path
    let target_path = expand_tilde(&path)?;

    // Collect all files to remove (recursively for directories)
    let target_files = collect_target_files(&target_path)?;
    if target_files.is_empty() {
        return Err(DottyError::InvalidTargetPath {
            path: target_path.display().to_string(),
            reason: "no files found at this path".into(),
        });
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
            // Use ancestors() for component-aware prefix matching.
            // String-based starts_with() produces false positives:
            // "~/.config/nvim" would incorrectly match "~/.config/nvim-legacy/file.lua".
            if let Ok(target) = repo_to_target(&repo_path_buf)
                && target.ancestors().any(|a| a == target_path)
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
        return Err(DottyError::InvalidTargetPath {
            path: target_path.display().to_string(),
            reason: "Path not managed by dotty".into(),
        });
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
        state_path: state_path.clone(),
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
fn collect_target_files(target_path: &Path) -> Result<Vec<PathBuf>, DottyError> {
    let mut files = Vec::new();

    if target_path.is_file() || is_symlink(target_path) {
        files.push(target_path.to_path_buf());
    } else if target_path.is_dir() {
        walk_dir(target_path, &mut files, 0)?;
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
    pub state_path: PathBuf,
    pub managed_pairs: Vec<(PathBuf, String)>,
    pub skipped: HashSet<String>,
    pub commit: Option<String>,
}

/// Output of `build_remove_plan`.
pub(crate) struct RemovePlanOutput {
    pub plan: Plan,
    pub config: Config,
}

/// Phase 1: Copy files from repo back to target (restore as regular files).
///
/// For each managed pair where the repo file exists and the file is not
/// skipped, adds a `Backup` action (if the target exists and is not a symlink)
/// followed by a `CopyFile` action. This phase runs before symlink removal
/// so that if CopyFile fails, the original symlink is still intact — no data
/// loss, and rollback can restore it.
///
/// The `Backup` action preserves user modifications before they are overwritten
/// by the repo version. Backups are stored in `<state_path>/backups/<timestamp>/<filename>`.
///
/// See: high-fix-remove-plan-phase-ordering
/// See: medium-add-remove-backup
pub(crate) fn build_restore_file_phase(input: &RemovePlanInput) -> Vec<Action> {
    let mut actions = Vec::new();
    for (target_file, repo_rel) in &input.managed_pairs {
        if input.skipped.contains(repo_rel) {
            continue;
        }
        let repo_file = input.repo_path.join(repo_rel);
        if repo_file.exists() {
            // Backup user modifications before overwriting with repo version.
            // Only backup regular files (not symlinks — those are handled in Phase 2).
            if target_file.exists() && !is_symlink(target_file) {
                let backup_ts = crate::backups::backup_timestamp();
                let backup_dest = input
                    .state_path
                    .join("backups")
                    .join(&backup_ts)
                    .join(target_file.file_name().unwrap_or_default());
                actions.push(Action::Backup {
                    source: target_file.clone(),
                    dest: backup_dest,
                });
            }
            // Skip CopyFile for symlinks pointing to directories. CopyFile
            // would remove the symlink and replace the directory with a
            // single file, orphaning the directory content. The symlink is
            // handled in Phase 2 (build_remove_symlink_phase).
            if is_symlink(target_file)
                && let Ok(target) = fs::read_link(target_file)
                && target.is_dir()
            {
                continue;
            }
            actions.push(Action::CopyFile {
                source: repo_file,
                dest: target_file.clone(),
            });
        }
    }
    actions
}

/// Phase 2: Remove symlinks at target locations.
///
/// For each managed pair where the target is a symlink and the file is not
/// skipped, adds a `RemoveSymlink` action.
pub(crate) fn build_remove_symlink_phase(input: &RemovePlanInput) -> Vec<Action> {
    let mut actions = Vec::new();
    for (target_file, repo_rel) in &input.managed_pairs {
        if input.skipped.contains(repo_rel) {
            continue;
        }
        if is_symlink(target_file) {
            actions.push(Action::RemoveSymlink {
                path: target_file.clone(),
            });
        }
    }
    actions
}

/// Phase 3: Remove files from repo, update config, and prepare git staging.
///
/// For each managed pair where the file is not skipped, adds a `RemoveFile`
/// action, removes the entry from the config's managed map, and collects the
/// repo-relative path for git staging.
pub(crate) fn build_repo_cleanup_phase(
    config: &mut Config,
    input: &RemovePlanInput,
) -> (Vec<Action>, Vec<PathBuf>) {
    let mut actions = Vec::new();
    let mut git_rm_paths = Vec::new();
    for (_target_file, repo_rel) in &input.managed_pairs {
        if input.skipped.contains(repo_rel) {
            continue;
        }
        let repo_file = input.repo_path.join(repo_rel);
        actions.push(Action::RemoveFile { path: repo_file });
        config.managed.shift_remove(repo_rel);
        git_rm_paths.push(PathBuf::from(repo_rel));
    }
    (actions, git_rm_paths)
}

/// Build a plan for removing files from the dotty repository.
///
/// This is a pure function: it takes resolved input data (managed pairs,
/// skipped files from user prompts) and returns a `Plan` with actions
/// and an updated `Config`. No filesystem or git operations are performed.
pub(crate) fn build_remove_plan(
    input: &RemovePlanInput,
    config: &Config,
) -> Result<RemovePlanOutput, DottyError> {
    let mut plan = Plan::new(&input.repo_path);
    let mut config = config.clone();

    // Phase 1: Restore files from repo
    plan.actions.extend(build_restore_file_phase(input));

    // Phase 2: Remove symlinks
    plan.actions.extend(build_remove_symlink_phase(input));

    // Phase 3: Repo cleanup (remove files, update config, collect git paths)
    let (cleanup_actions, git_rm_paths) = build_repo_cleanup_phase(&mut config, input);
    plan.actions.extend(cleanup_actions);

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
) -> Result<HashSet<String>, DottyError> {
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

    // -- prefix matching tests --

    /// Verify that `ancestors()`-based matching correctly handles edge cases.
    /// This is the logic used in the directory removal block of `run()`.
    fn is_target_under_path(target: &Path, target_path: &Path) -> bool {
        target.ancestors().any(|a| a == target_path)
    }

    #[test]
    fn test_prefix_match_exact() {
        let target = Path::new("~/.config/nvim");
        let target_path = Path::new("~/.config/nvim");
        assert!(
            is_target_under_path(target, target_path),
            "exact match should match"
        );
    }

    #[test]
    fn test_prefix_match_subdirectory() {
        let target = Path::new("~/.config/nvim/init.lua");
        let target_path = Path::new("~/.config/nvim");
        assert!(
            is_target_under_path(target, target_path),
            "subdirectory should match"
        );
    }

    #[test]
    fn test_prefix_match_false_positive_sibling() {
        let target = Path::new("~/.config/nvim-legacy/file.lua");
        let target_path = Path::new("~/.config/nvim");
        assert!(
            !is_target_under_path(target, target_path),
            "nvim-legacy should NOT match nvim (false positive)"
        );
    }

    #[test]
    fn test_prefix_match_false_positive_longer_prefix() {
        let target = Path::new("~/.config/nvimrc");
        let target_path = Path::new("~/.config/nvim");
        assert!(
            !is_target_under_path(target, target_path),
            "nvimrc should NOT match nvim (false positive)"
        );
    }

    #[test]
    fn test_prefix_match_deeply_nested() {
        let target = Path::new("~/.config/nvim/lua/plugins/my-plugin/init.lua");
        let target_path = Path::new("~/.config/nvim");
        assert!(
            is_target_under_path(target, target_path),
            "deeply nested file should match"
        );
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
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // Backup + CopyFile + RemoveFile + GitAdd = 4 actions (target exists as regular file)
        assert_eq!(output.plan.actions.len(), 4);
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
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // CopyFile + RemoveSymlink + RemoveFile + GitAdd = 4 actions (target is symlink, no backup)
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
            state_path: state.clone(),
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
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: Some("remove vimrc".to_string()),
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // Backup + CopyFile + RemoveFile + GitAdd + GitCommit = 5
        assert_eq!(output.plan.actions.len(), 5);

        match &output.plan.actions.last().unwrap() {
            Action::GitCommit { message } => assert_eq!(message, "remove vimrc"),
            other => panic!("expected GitCommit, got: {other:?}"),
        }
    }

    /// Regression test: Phase 1 (CopyFile) executes before Phase 2 (RemoveSymlink).
    ///
    /// If Phase 1 succeeds but Phase 2 fails, the target file exists (restored from
    /// repo) — no data loss. This is the key safety property of the phase ordering.
    #[test]
    fn test_build_remove_plan_phase_ordering_copy_before_symlink() {
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
        std::fs::write(&repo_file, "repo content").unwrap();
        crate::symlink::create_symlink(&repo_file, &target).unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // Verify phase ordering: CopyFile must come before RemoveSymlink (target is symlink, no backup)
        let mut found_copy = false;
        let mut found_remove_symlink = false;

        for action in &output.plan.actions {
            match action {
                Action::CopyFile { .. } => {
                    assert!(
                        !found_remove_symlink,
                        "CopyFile must come before RemoveSymlink to prevent data loss"
                    );
                    found_copy = true;
                }
                Action::RemoveSymlink { .. } => {
                    found_remove_symlink = true;
                }
                _ => {}
            }
        }

        assert!(found_copy, "expected CopyFile action");
        assert!(found_remove_symlink, "expected RemoveSymlink action");
    }

    /// Verify that skipped files are excluded from both CopyFile and RemoveSymlink.
    #[test]
    fn test_build_remove_plan_skipped_excludes_both_phases() {
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
        std::fs::write(&repo_file, "repo content").unwrap();
        crate::symlink::create_symlink(&repo_file, &target).unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let mut skipped = HashSet::new();
        skipped.insert("base/home/.vimrc".to_string());

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped,
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // Skipped: no CopyFile, no RemoveSymlink, no RemoveFile
        assert!(output.plan.is_empty());
        assert!(output.config.managed.contains_key("base/home/.vimrc"));
    }

    // -- phase extraction unit tests --

    #[test]
    fn test_build_restore_file_phase_with_repo_file() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        // Target file doesn't exist, so no Backup — only CopyFile = 1 action
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::CopyFile { source, dest } => {
                assert_eq!(source, &repo_file);
                assert_eq!(dest, &target);
            }
            other => panic!("expected CopyFile, got: {:?}", other),
        }
    }

    #[test]
    fn test_build_restore_file_phase_missing_repo_file() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        // Repo file doesn't exist, so no CopyFile action
        assert!(actions.is_empty());
    }

    #[test]
    fn test_build_restore_file_phase_skipped() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();

        let mut skipped = HashSet::new();
        skipped.insert("base/home/.vimrc".to_string());

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped,
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        assert!(actions.is_empty());
    }

    #[test]
    fn test_build_remove_symlink_phase_with_symlink() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();
        crate::symlink::create_symlink(&repo_file, &target).unwrap();

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_remove_symlink_phase(&input);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::RemoveSymlink { path } => assert_eq!(path, &target),
            other => panic!("expected RemoveSymlink, got: {:?}", other),
        }
    }

    #[test]
    fn test_build_remove_symlink_phase_not_a_symlink() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        std::fs::write(&target, "content").unwrap();

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_remove_symlink_phase(&input);

        // Target is a regular file, not a symlink
        assert!(actions.is_empty());
    }

    #[test]
    fn test_build_remove_symlink_phase_skipped() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();
        crate::symlink::create_symlink(&repo_file, &target).unwrap();

        let mut skipped = HashSet::new();
        skipped.insert("base/home/.vimrc".to_string());

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped,
            commit: None,
        };
        let actions = build_remove_symlink_phase(&input);

        assert!(actions.is_empty());
    }

    #[test]
    fn test_build_repo_cleanup_phase() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let (actions, git_paths) = build_repo_cleanup_phase(&mut config, &input);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::RemoveFile { path } => assert_eq!(path, &repo_file),
            other => panic!("expected RemoveFile, got: {:?}", other),
        }
        assert_eq!(git_paths, vec![PathBuf::from("base/home/.vimrc")]);
        assert!(!config.managed.contains_key("base/home/.vimrc"));
    }

    #[test]
    fn test_build_repo_cleanup_phase_skipped() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        let mut skipped = HashSet::new();
        skipped.insert("base/home/.vimrc".to_string());

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped,
            commit: None,
        };
        let (actions, git_paths) = build_repo_cleanup_phase(&mut config, &input);

        assert!(actions.is_empty());
        assert!(git_paths.is_empty());
        // Config should be unchanged for skipped files
        assert!(config.managed.contains_key("base/home/.vimrc"));
    }

    #[test]
    fn test_build_repo_cleanup_phase_multiple_files() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let vimrc_target = home.join(".vimrc");
        let vimrc_repo = repo.join("base/home/.vimrc");
        let nvim_target = home.join(".config/nvim/init.lua");
        let nvim_repo = repo.join("base/home/.config/nvim/init.lua");
        std::fs::create_dir_all(vimrc_repo.parent().unwrap()).unwrap();
        std::fs::create_dir_all(nvim_repo.parent().unwrap()).unwrap();
        std::fs::write(&vimrc_repo, "vimrc").unwrap();
        std::fs::write(&nvim_repo, "init").unwrap();

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());
        config.managed.insert(
            "base/home/.config/nvim/init.lua".into(),
            "~/.config/nvim/init.lua".into(),
        );

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![
                (vimrc_target.clone(), "base/home/.vimrc".to_string()),
                (
                    nvim_target.clone(),
                    "base/home/.config/nvim/init.lua".to_string(),
                ),
            ],
            skipped: HashSet::new(),
            commit: None,
        };
        let (actions, git_paths) = build_repo_cleanup_phase(&mut config, &input);

        assert_eq!(actions.len(), 2);
        assert_eq!(git_paths.len(), 2);
        assert_eq!(config.managed.len(), 0);
    }

    /// Test that Backup action is created before CopyFile when target exists
    /// as a regular file (not a symlink).
    #[test]
    fn test_build_restore_file_phase_with_target_file_creates_backup() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        std::fs::write(&target, "user modified content").unwrap();

        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "repo content").unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        // Backup + CopyFile = 2 actions
        assert_eq!(actions.len(), 2);

        // First action should be Backup
        match &actions[0] {
            Action::Backup { source, dest } => {
                assert_eq!(source, &target);
                assert!(dest.starts_with(&state));
                assert!(dest.to_string_lossy().contains("backups"));
                assert!(dest.to_string_lossy().contains(".vimrc"));
            }
            other => panic!("expected Backup, got: {:?}", other),
        }

        // Second action should be CopyFile
        match &actions[1] {
            Action::CopyFile { source, dest } => {
                assert_eq!(source, &repo_file);
                assert_eq!(dest, &target);
            }
            other => panic!("expected CopyFile, got: {:?}", other),
        }
    }

    /// Test that no Backup action is created when the target is a symlink.
    #[test]
    fn test_build_restore_file_phase_symlink_no_backup() {
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
        std::fs::write(&repo_file, "repo content").unwrap();
        crate::symlink::create_symlink(&repo_file, &target).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        // Only CopyFile = 1 action (no backup for symlinks)
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::CopyFile { source, dest } => {
                assert_eq!(source, &repo_file);
                assert_eq!(dest, &target);
            }
            other => panic!("expected CopyFile, got: {:?}", other),
        }
    }

    /// Test that no Backup action is created when the target file doesn't exist.
    #[test]
    fn test_build_restore_file_phase_no_target_no_backup() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        // Target does NOT exist

        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "repo content").unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        // Only CopyFile = 1 action (no backup when target doesn't exist)
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::CopyFile { source, dest } => {
                assert_eq!(source, &repo_file);
                assert_eq!(dest, &target);
            }
            other => panic!("expected CopyFile, got: {:?}", other),
        }
    }

    /// Test that CopyFile is skipped when the target is a symlink to a
    /// directory. CopyFile would remove the symlink and replace the directory
    /// with a single file, orphaning the directory content.
    #[test]
    fn test_build_restore_file_phase_skips_symlink_to_directory() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        // Create a directory in the target location (simulating the symlink target)
        let target_dir = home.join("managed_dir");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("file.txt"), "content").unwrap();

        // Create a symlink pointing to the directory
        let symlink = home.join("link_to_dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_dir, &symlink).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&target_dir, &symlink).unwrap();

        // Create a repo file
        let repo_file = repo.join("base/home/link_to_dir/file.txt");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "repo content").unwrap();

        let state = base.join("state");
        std::fs::create_dir_all(&state).unwrap();

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(
                symlink.clone(),
                "base/home/link_to_dir/file.txt".to_string(),
            )],
            skipped: HashSet::new(),
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        // No CopyFile (target is symlink to dir), no Backup
        assert!(actions.is_empty());
    }

    /// Test the full remove plan when target is a symlink to a directory.
    ///
    /// Verifies that Phase 1 skips CopyFile (preventing directory replacement)
    /// and Phase 2 still emits RemoveSymlink.
    #[test]
    fn test_build_remove_plan_with_symlink_to_directory() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        // Create a directory in the target location
        let target_dir = home.join("managed_dir");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("file.txt"), "content").unwrap();

        // Create a symlink pointing to the directory
        let symlink = home.join("link_to_dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_dir, &symlink).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&target_dir, &symlink).unwrap();

        // Create a repo file
        let repo_file = repo.join("base/home/link_to_dir/file.txt");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "repo content").unwrap();

        let mut config = Config::new();
        config.managed.insert(
            "base/home/link_to_dir/file.txt".into(),
            "~/.link_to_dir".into(),
        );

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(
                symlink.clone(),
                "base/home/link_to_dir/file.txt".to_string(),
            )],
            skipped: HashSet::new(),
            commit: None,
        };
        let output = build_remove_plan(&input, &config).unwrap();

        // No CopyFile (target is symlink to dir), but RemoveSymlink + RemoveFile + GitAdd
        assert!(
            !output
                .plan
                .actions
                .iter()
                .any(|a| matches!(a, Action::CopyFile { .. })),
            "CopyFile should not be generated for symlink-to-directory"
        );
        assert!(
            output
                .plan
                .actions
                .iter()
                .any(|a| matches!(a, Action::RemoveSymlink { .. })),
            "RemoveSymlink should be generated for symlink-to-directory"
        );
        assert!(
            output
                .plan
                .actions
                .iter()
                .any(|a| matches!(a, Action::RemoveFile { .. })),
            "RemoveFile should be generated for repo file"
        );
    }

    /// Test that skipped files produce no actions in build_restore_file_phase.
    #[test]
    fn test_build_restore_file_phase_skipped_no_actions() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        std::fs::write(&target, "user content").unwrap();

        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "repo content").unwrap();

        let mut skipped = HashSet::new();
        skipped.insert("base/home/.vimrc".to_string());

        let input = RemovePlanInput {
            repo_path: repo.clone(),
            state_path: state.clone(),
            managed_pairs: vec![(target.clone(), "base/home/.vimrc".to_string())],
            skipped,
            commit: None,
        };
        let actions = build_restore_file_phase(&input);

        assert!(actions.is_empty());
    }
}
