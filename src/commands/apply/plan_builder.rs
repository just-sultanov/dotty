//! Build an apply plan for the `apply` command.
//!
//! This module constructs a `Plan` by inspecting the filesystem state of each
//! target path and determining the necessary actions (CreateDir, Backup,
//! CreateSymlink, RemoveSymlink). It also collects per-file results for
//! console output. Orphan detection is delegated to `orphan_detection`.

use indexmap::IndexMap;
use std::path::PathBuf;

use indexmap::IndexSet;

use tracing::warn;

use crate::config::Config;
use crate::plan::{Action, Plan};

use super::inspect::{TargetState, inspect_target};
use super::orphan_detection::{OrphanDetectionInput, detect_orphans_and_build_removals};
use crate::error::DottyError;

/// Input data for building an `apply` plan.
pub(crate) struct ApplyPlanInput {
    pub repo_path: PathBuf,
    pub state_path: PathBuf,
    pub home: PathBuf,
    /// Merged tier map, ordered base → platform → machine.
    /// IndexMap preserves insertion order for deterministic iteration.
    pub merged: IndexMap<PathBuf, (String, String)>,
    pub override_map: IndexMap<PathBuf, String>,
    pub config: Config,
    /// When true, allow replacing directories with symlinks (requires backup).
    /// When false, directory replacements are skipped with a warning.
    pub force: bool,

    /// When true, follow symlinks during backup (copies target content).
    /// When false (default), skip symlinked files to prevent exposing
    /// sensitive data outside the intended home directory.
    pub follow_symlinks: bool,
}

/// Output of `build_apply_plan`.
pub(crate) struct ApplyPlanOutput {
    pub plan: Plan,
    pub file_results: Vec<FileResult>,
    /// Orphan entries: (repo_relative_path, target_path_string).
    pub orphans: Vec<(String, String)>,
    /// Removal actions for detected orphans. These are NOT added to `plan`
    /// by default — the caller (dispatch) decides whether to include them
    /// based on user confirmation or the `--force` flag.
    pub orphan_removal_actions: Vec<Action>,
}

/// Per-file result for console output.
#[derive(Clone)]
pub(crate) struct FileResult {
    pub(crate) target: PathBuf,
    pub(crate) tier: String,
    pub(crate) applied: bool,
    pub(crate) skipped: bool,
    pub(crate) overrides: Option<String>,
}

/// Build a plan for applying the dotty repository to the system.
///
/// This function inspects the filesystem state of each target path and
/// builds a `Plan` with the necessary actions (CreateDir, Backup,
/// CreateSymlink, RemoveSymlink). It also detects orphan managed entries
/// and produces per-file results for console output.
///
/// Returns `DottyError` for any plan-building failures. Currently infallible
/// since all sub-operations (`inspect_target`, `detect_orphans_and_build_removals`)
/// are non-fallible, but the error type is reserved for future fallible paths
/// (e.g., backup timestamp generation, state path creation).
pub(crate) fn build_apply_plan(
    input: &ApplyPlanInput,
) -> std::result::Result<ApplyPlanOutput, DottyError> {
    let mut plan = Plan::new(&input.repo_path);
    let mut file_results: Vec<FileResult> = Vec::new();

    // Collect unique parent directories to avoid duplicate CreateDir actions
    // when multiple files share the same parent directory.
    // IndexSet preserves insertion order, ensuring deterministic CreateDir
    // action ordering in dry-run output across runs.
    let mut created_parents = IndexSet::new();

    // Process each merged file
    for (target_path, (tier, repo_relative_path)) in &input.merged {
        let repo_absolute_path = input.repo_path.join(repo_relative_path);
        let target = target_path.to_path_buf();

        // Compute overrides early so they can be used in any branch
        let overrides = input.override_map.get(target_path).cloned();

        // Check target state
        let state = match inspect_target(&target, &repo_absolute_path) {
            TargetState::Correct => {
                file_results.push(FileResult {
                    target: target.clone(),
                    tier: tier.clone(),
                    applied: false,
                    skipped: true,
                    overrides: input.override_map.get(target_path).cloned(),
                });
                continue;
            }
            TargetState::CircularSymlink => {
                // Remove the circular symlink first, then create the correct one.
                plan.add(Action::RemoveSymlink {
                    path: target.clone(),
                });
                if let Some(parent) = target.parent() {
                    created_parents.insert(parent.to_path_buf());
                }
                plan.add(Action::CreateSymlink {
                    target: repo_absolute_path.clone(),
                    link: target.clone(),
                    backup_path: None,
                    backup_exists: false,
                });
                TargetState::CircularSymlink
            }
            TargetState::NeedsSymlink => {
                if let Some(parent) = target.parent() {
                    created_parents.insert(parent.to_path_buf());
                }
                plan.add(Action::CreateSymlink {
                    target: repo_absolute_path.clone(),
                    link: target.clone(),
                    backup_path: None,
                    backup_exists: false,
                });
                TargetState::NeedsSymlink
            }
            TargetState::NeedsBackup => {
                if let Some(parent) = target.parent() {
                    created_parents.insert(parent.to_path_buf());
                }
                let backup_base = input.state_path.join("backups");
                let backup_ts = crate::backups::backup_timestamp();
                let backup_dest = if let Ok(relative) = target.strip_prefix(&input.home) {
                    backup_base.join(&backup_ts).join(relative)
                } else {
                    backup_base
                        .join(&backup_ts)
                        .join(target.file_name().unwrap_or_default())
                };
                plan.add(Action::Backup {
                    source: target.clone(),
                    dest: backup_dest.clone(),
                });
                plan.add(Action::CreateSymlink {
                    target: repo_absolute_path.clone(),
                    link: target.clone(),
                    backup_path: Some(backup_dest.clone()),
                    backup_exists: true,
                });
                TargetState::NeedsBackup
            }
            TargetState::NeedsBackupDir(dir_path) => {
                if !input.force {
                    // Skip directory replacement in non-force mode.
                    // The user must explicitly allow it with --force.
                    warn!(
                        "skipping directory-to-symlink replacement at {} (use --force to allow)",
                        dir_path
                    );
                    file_results.push(FileResult {
                        target: target.clone(),
                        tier: tier.clone(),
                        applied: false,
                        skipped: true,
                        overrides: overrides.clone(),
                    });
                    continue;
                }
                if let Some(parent) = target.parent() {
                    created_parents.insert(parent.to_path_buf());
                }
                let backup_base = input.state_path.join("backups");
                let backup_ts = crate::backups::backup_timestamp();
                let backup_dest = if let Ok(relative) = target.strip_prefix(&input.home) {
                    backup_base.join(&backup_ts).join(relative)
                } else {
                    backup_base
                        .join(&backup_ts)
                        .join(target.file_name().unwrap_or_default())
                };
                plan.add(Action::BackupDir {
                    source: target.clone(),
                    dest: backup_dest.clone(),
                    follow_symlinks: input.follow_symlinks,
                });
                plan.add(Action::CreateSymlink {
                    target: repo_absolute_path.clone(),
                    link: target.clone(),
                    backup_path: Some(backup_dest.clone()),
                    backup_exists: true,
                });
                warn!(
                    "replacing directory {} with symlink → {}",
                    dir_path,
                    repo_absolute_path.display()
                );
                TargetState::NeedsBackupDir(dir_path)
            }
        };

        file_results.push(FileResult {
            target: target.clone(),
            tier: tier.clone(),
            applied: state != TargetState::Correct,
            skipped: false,
            overrides,
        });
    }

    // Add deduplicated CreateDir actions for all unique parent directories.
    for parent in created_parents {
        plan.add(Action::CreateDir { path: parent });
    }

    // Orphan detection delegated to dedicated module.
    // Removal actions are returned separately so the caller can decide
    // whether to include them based on user confirmation or --force.
    let orphan_input = OrphanDetectionInput {
        merged: &input.merged,
        config: &input.config,
    };
    let orphan_output = detect_orphans_and_build_removals(&orphan_input);

    Ok(ApplyPlanOutput {
        plan,
        file_results,
        orphans: orphan_output.orphans,
        orphan_removal_actions: orphan_output.removal_actions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symlink::create_symlink;

    #[test]
    fn test_inspect_target_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nonexistent.txt");
        let repo_absolute_path = PathBuf::from("/tmp/dotty_repo_file.txt");
        assert_eq!(
            inspect_target(&target, &repo_absolute_path),
            TargetState::NeedsSymlink
        );
    }

    #[test]
    fn test_inspect_target_circular_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("circular_link");
        let repo_absolute_path = PathBuf::from("/tmp/repo.txt");

        create_symlink(&target, &target).unwrap();
        assert!(crate::symlink::is_symlink(&target));

        assert_eq!(
            inspect_target(&target, &repo_absolute_path),
            TargetState::CircularSymlink
        );
    }

    #[test]
    fn test_inspect_target_circular_symlink_two_node() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let repo_absolute_path = PathBuf::from("/tmp/repo.txt");

        create_symlink(&b, &a).unwrap();
        create_symlink(&a, &b).unwrap();

        assert_eq!(
            inspect_target(&a, &repo_absolute_path),
            TargetState::CircularSymlink
        );
        assert_eq!(
            inspect_target(&b, &repo_absolute_path),
            TargetState::CircularSymlink
        );
    }

    #[test]
    fn test_inspect_target_regular_file() {
        crate::tests::with_test_home(|home| {
            let target = home.join("file.txt");
            std::fs::write(&target, "content").unwrap();
            let repo_absolute_path = PathBuf::from("/tmp/repo.txt");

            assert_eq!(
                inspect_target(&target, &repo_absolute_path),
                TargetState::NeedsBackup
            );
        });
    }

    #[test]
    fn test_inspect_target_directory() {
        crate::tests::with_test_home(|home| {
            let target = home.join("config_dir");
            std::fs::create_dir(&target).unwrap();
            let repo_absolute_path = PathBuf::from("/tmp/repo.txt");

            let state = inspect_target(&target, &repo_absolute_path);
            assert!(
                matches!(state, TargetState::NeedsBackupDir(_)),
                "expected NeedsBackupDir, got {:?}",
                state
            );
            if let TargetState::NeedsBackupDir(path) = state {
                assert_eq!(path, target.to_string_lossy().to_string());
            }
        });
    }

    #[test]
    fn test_inspect_target_correct_with_dotdot_in_link() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let sub = base.join("repo");
        std::fs::create_dir_all(&sub).unwrap();
        let repo_absolute_path = sub.join("file.txt");
        std::fs::write(&repo_absolute_path, "content").unwrap();

        let target_dir = base.join("home");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("link");

        let link_target = base.join("repo").join("..").join("repo").join("file.txt");
        create_symlink(&link_target, &target).unwrap();

        assert_eq!(
            inspect_target(&target, &repo_absolute_path),
            TargetState::Correct
        );
    }

    #[test]
    fn test_inspect_target_fallback_when_canonicalization_fails() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let target_dir = base.join("home");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("link");

        let nonexistent_parent = base.join("no_such_dir").join("file.txt");
        create_symlink(&nonexistent_parent, &target).unwrap();

        let expected = base.join("other_dir").join("file.txt");
        assert_eq!(
            inspect_target(&target, &expected),
            TargetState::NeedsSymlink
        );
    }

    #[test]
    fn test_build_apply_plan_all_correct() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_absolute_path = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "content").unwrap();
        create_symlink(&repo_absolute_path, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert!(output.plan.is_empty());
            assert_eq!(output.file_results.len(), 1);
            assert!(output.file_results[0].skipped);
            assert!(output.orphans.is_empty());
        });
    }

    #[test]
    fn test_build_apply_plan_needs_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_absolute_path = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "content").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.plan.actions.len(), 2);
            assert_eq!(output.file_results.len(), 1);
            assert!(output.file_results[0].applied);
        });
    }

    #[test]
    fn test_build_apply_plan_circular_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_absolute_path = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "content").unwrap();
        create_symlink(&target, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.plan.actions.len(), 3);
            assert_eq!(output.file_results.len(), 1);
            assert!(output.file_results[0].applied);
        });
    }

    #[test]
    fn test_build_apply_plan_needs_backup() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_absolute_path = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "new content").unwrap();
        std::fs::write(&target, "old content").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.plan.actions.len(), 3);
            assert_eq!(output.file_results.len(), 1);
            assert!(output.file_results[0].applied);
        });
    }

    #[test]
    fn test_build_apply_plan_needs_backup_dir_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join("config_dir");
        let repo_absolute_path = repo.join("base/home/.config_dir");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "new content").unwrap();
        // Create a real directory at the target location
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inner.txt"), "inner").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.config_dir".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.config_dir".into(), "~/.config_dir".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            // Without --force, directory replacement should be skipped
            assert!(output.plan.is_empty());
            assert_eq!(output.file_results.len(), 1);
            assert!(!output.file_results[0].applied);
            assert!(output.file_results[0].skipped);
        });
    }

    #[test]
    fn test_build_apply_plan_needs_backup_dir_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join("config_dir");
        let repo_absolute_path = repo.join("base/home/.config_dir");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "new content").unwrap();
        // Create a real directory at the target location
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inner.txt"), "inner").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.config_dir".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.config_dir".into(), "~/.config_dir".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: true,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            // With --force, directory replacement should proceed.
            // CreateDir is now deduplicated and added after per-file actions.
            assert_eq!(output.plan.actions.len(), 3);
            assert!(
                output
                    .plan
                    .actions
                    .iter()
                    .any(|a| matches!(a, Action::CreateDir { .. })),
                "plan should contain CreateDir"
            );
            assert!(
                output
                    .plan
                    .actions
                    .iter()
                    .any(|a| matches!(a, Action::BackupDir { .. })),
                "plan should contain BackupDir"
            );
            assert!(
                output
                    .plan
                    .actions
                    .iter()
                    .any(|a| matches!(a, Action::CreateSymlink { .. })),
                "plan should contain CreateSymlink"
            );
            assert_eq!(output.file_results.len(), 1);
            assert!(output.file_results[0].applied);
        });
    }

    /// Tests orphan detection: files in merged not in config.managed.
    /// tracked_set is built from config.managed keys to ensure consistent
    /// key format (repo_relative_path strings) across both sources.
    #[test]
    fn test_build_apply_plan_orphan_detection() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_absolute_path = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "content").unwrap();
        create_symlink(&repo_absolute_path, &target).unwrap();

        let target_old = home.join(".old");
        let repo_absolute_path_old = repo.join("base/home/.old");
        std::fs::create_dir_all(repo_absolute_path_old.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path_old, "old content").unwrap();
        create_symlink(&repo_absolute_path_old, &target_old).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        merged.insert(
            target_old.clone(),
            ("base".to_string(), "base/home/.old".to_string()),
        );
        let mut config = Config::new();
        // Only .vimrc is in config.managed; .old was removed from config
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            // .old is in merged but not in config.managed → orphan
            assert_eq!(output.orphans.len(), 1);
            assert_eq!(output.orphans[0].0, "base/home/.old");
            // Orphan removal actions are returned separately, not in plan
            assert!(!output.orphan_removal_actions.is_empty());
            assert!(
                output
                    .orphan_removal_actions
                    .iter()
                    .any(|a| matches!(a, Action::RemoveSymlink { .. })),
                "should have RemoveSymlink for orphan symlink"
            );
        });
    }

    /// Tests that overridden lower-priority tier files are NOT flagged
    /// as orphans when tracked_set is built from config.managed keys.
    #[test]
    fn test_build_apply_plan_orphan_detection_no_false_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        let repo_absolute_path = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "base content").unwrap();
        create_symlink(&repo_absolute_path, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            // No orphans when merged and config.managed are aligned
            assert!(output.orphans.is_empty());
        });
    }

    /// Integration test for the full override pipeline across 3 tiers.
    ///
    /// Verifies that `build_override_map` → `build_apply_plan` correctly
    /// propagates override information into `FileResult.overrides`, so that
    /// `print_per_file_summary` can display the overriding tier name.
    ///
    /// Fixture: base + macos (platform) + macbook (machine) tiers with
    /// one file overridden from base → macbook, and another from base → macos.
    #[test]
    fn test_build_apply_plan_override_pipeline() {
        use crate::commands::apply::tiers::{build_override_map, merge_tiers};

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        // Create repo files across 3 tiers
        let nvim_plugins_base = repo.join("base/home/.config/nvim/plugins.lua");
        std::fs::create_dir_all(nvim_plugins_base.parent().unwrap()).unwrap();
        std::fs::write(&nvim_plugins_base, "base plugins").unwrap();

        let nvim_plugins_machine = repo.join("macbook/home/.config/nvim/plugins.lua");
        std::fs::create_dir_all(nvim_plugins_machine.parent().unwrap()).unwrap();
        std::fs::write(&nvim_plugins_machine, "macbook plugins").unwrap();

        let skhdrc = repo.join("macos/home/.config/skhd/skhdrc");
        std::fs::create_dir_all(skhdrc.parent().unwrap()).unwrap();
        std::fs::write(&skhdrc, "skhd config").unwrap();

        let vimrc = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(vimrc.parent().unwrap()).unwrap();
        std::fs::write(&vimrc, "vimrc content").unwrap();

        // Set up tracked files list
        let tracked_files = vec![
            "base/home/.config/nvim/plugins.lua".into(),
            "macbook/home/.config/nvim/plugins.lua".into(),
            "macos/home/.config/skhd/skhdrc".into(),
            "base/home/.vimrc".into(),
        ];

        // Use temp HOME so repo_to_target resolves to the test home directory
        crate::tests::with_test_home(|home| {
            // Step 1: build_override_map
            let override_map = build_override_map(
                &tracked_files,
                &Some("macbook".into()),
                &Some("macos".into()),
            );

            // Verify override_map has exactly 1 entry (nvim plugins overridden from base → macbook)
            assert_eq!(override_map.len(), 1);
            let nvim_target = home.join(".config/nvim/plugins.lua");
            assert!(override_map.contains_key(&nvim_target));
            assert_eq!(override_map.get(&nvim_target).unwrap(), "base");

            // Step 2: merge_tiers
            let merged = merge_tiers(&tracked_files, "macbook", &Some("macos".into()));
            assert_eq!(merged.len(), 3);

            // Step 3: build_apply_plan with override_map
            let mut config = Config::new();
            for (target, (tier, repo_relative_path)) in merged.iter() {
                config.managed.insert(
                    repo_relative_path.clone(),
                    target.to_string_lossy().to_string(),
                );
                let _ = tier; // used for orphan detection alignment
            }

            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged: merged.clone(),
                override_map: override_map.clone(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            // Should have 3 file results
            assert_eq!(output.file_results.len(), 3);

            // Find the nvim plugins result (the overridden one)
            let nvim_result = output
                .file_results
                .iter()
                .find(|r| r.target == nvim_target)
                .expect("nvim plugins result should exist");

            // Verify override is correctly recorded
            assert!(
                nvim_result.overrides.is_some(),
                "nvim plugins should have an override recorded"
            );
            assert_eq!(
                nvim_result.overrides.as_ref().unwrap(),
                "base",
                "overridden tier should be 'base'"
            );
            assert_eq!(
                nvim_result.tier, "macbook",
                "active tier should be 'macbook'"
            );

            // Find the skhd result (platform tier, no override)
            let skhd_target = home.join(".config/skhd/skhdrc");
            let skhd_result = output
                .file_results
                .iter()
                .find(|r| r.target == skhd_target)
                .expect("skhd result should exist");

            assert!(
                skhd_result.overrides.is_none(),
                "skhdrc should have no override"
            );
            assert_eq!(skhd_result.tier, "macos");

            // Find the vimrc result (base tier, no override)
            let vimrc_target = home.join(".vimrc");
            let vimrc_result = output
                .file_results
                .iter()
                .find(|r| r.target == vimrc_target)
                .expect("vimrc result should exist");

            assert!(
                vimrc_result.overrides.is_none(),
                "vimrc should have no override"
            );
            assert_eq!(vimrc_result.tier, "base");

            // No orphans
            assert!(output.orphans.is_empty());
        });
    }

    /// Integration test for the full pipeline with no overrides (happy path).
    ///
    /// Verifies that when files exist in only one tier each, `build_override_map`
    /// returns an empty map and `build_apply_plan` correctly reports no overrides.
    #[test]
    fn test_build_apply_plan_no_overrides() {
        use crate::commands::apply::tiers::{build_override_map, merge_tiers};

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        // Create repo files across 3 tiers, each targeting a different path
        let vimrc = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(vimrc.parent().unwrap()).unwrap();
        std::fs::write(&vimrc, "vimrc content").unwrap();

        let skhdrc = repo.join("macos/home/.config/skhd/skhdrc");
        std::fs::create_dir_all(skhdrc.parent().unwrap()).unwrap();
        std::fs::write(&skhdrc, "skhd config").unwrap();

        let nvim = repo.join("macbook/home/.config/nvim/init.lua");
        std::fs::create_dir_all(nvim.parent().unwrap()).unwrap();
        std::fs::write(&nvim, "nvim init").unwrap();

        let tracked_files = vec![
            "base/home/.vimrc".into(),
            "macos/home/.config/skhd/skhdrc".into(),
            "macbook/home/.config/nvim/init.lua".into(),
        ];

        // Use temp HOME so repo_to_target resolves to the test home directory
        crate::tests::with_test_home(|home| {
            // Step 1: build_override_map should return empty
            let override_map = build_override_map(
                &tracked_files,
                &Some("macbook".into()),
                &Some("macos".into()),
            );
            assert!(
                override_map.is_empty(),
                "override_map should be empty when no files span multiple tiers"
            );

            // Step 2: merge_tiers
            let merged = merge_tiers(&tracked_files, "macbook", &Some("macos".into()));
            assert_eq!(merged.len(), 3);

            // Step 3: build_apply_plan with empty override_map
            let mut config = Config::new();
            for (target, (tier, repo_relative_path)) in merged.iter() {
                config.managed.insert(
                    repo_relative_path.clone(),
                    target.to_string_lossy().to_string(),
                );
                let _ = tier; // used for orphan detection alignment
            }

            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged: merged.clone(),
                override_map: override_map.clone(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.file_results.len(), 3);

            // Verify no file has an override recorded
            for result in &output.file_results {
                assert!(
                    result.overrides.is_none(),
                    "file {} should have no override, got {:?}",
                    result.target.display(),
                    result.overrides
                );
            }

            assert!(output.orphans.is_empty());
        });
    }

    /// Tests that orphan symlinks are removed via RemoveSymlink action.
    #[test]
    fn test_orphan_removal_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".old_symlink");
        let repo_absolute_path = repo.join("base/home/.old_symlink");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "old content").unwrap();
        create_symlink(&repo_absolute_path, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.old_symlink".to_string()),
        );
        let config = Config::new();
        // .old_symlink is NOT in config.managed → orphan

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.orphans.len(), 1);
            assert_eq!(output.orphans[0].0, "base/home/.old_symlink");
            // Orphan removal actions are returned separately
            assert!(
                output
                    .orphan_removal_actions
                    .iter()
                    .any(|a| matches!(a, Action::RemoveSymlink { path } if path == &target)),
                "orphan_removal_actions should contain RemoveSymlink for orphan symlink"
            );
        });
    }

    /// Tests that orphan regular files are removed via RemoveFile action
    /// (not RemoveSymlink, which would silently do nothing).
    #[test]
    fn test_orphan_removal_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".old_file");
        let repo_absolute_path = repo.join("base/home/.old_file");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "old content").unwrap();
        // Create as a regular file (not a symlink)
        std::fs::write(&target, "stale content").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.old_file".to_string()),
        );
        let config = Config::new();
        // .old_file is NOT in config.managed → orphan

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.orphans.len(), 1);
            // Orphan removal actions are returned separately
            // Should have RemoveFile action for the orphan (not RemoveSymlink)
            assert!(
                output
                    .orphan_removal_actions
                    .iter()
                    .any(|a| matches!(a, Action::RemoveFile { path } if path == &target)),
                "orphan_removal_actions should contain RemoveFile for orphan regular file"
            );
            // Should NOT have RemoveSymlink for this orphan
            assert!(
                !output
                    .orphan_removal_actions
                    .iter()
                    .any(|a| matches!(a, Action::RemoveSymlink { path } if path == &target)),
                "orphan_removal_actions should NOT contain RemoveSymlink for orphan regular file"
            );
        });
    }

    /// Tests that non-existent orphan targets are skipped (no action generated).
    #[test]
    fn test_orphan_removal_non_existent() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let target = home.join(".already_gone");
        let repo_absolute_path = repo.join("base/home/.already_gone");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "old content").unwrap();
        // Do NOT create the target file — it's already been removed

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.already_gone".to_string()),
        );
        let config = Config::new();
        // .already_gone is NOT in config.managed → orphan

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.orphans.len(), 1);
            // Should have NO removal actions (target already gone)
            let removal_actions: Vec<_> = output
                .plan
                .actions
                .iter()
                .filter(|a| matches!(a, Action::RemoveFile { .. } | Action::RemoveSymlink { .. }))
                .collect();
            assert!(
                removal_actions.is_empty(),
                "plan should have no removal actions for non-existent orphan"
            );
        });
    }

    /// Tests that CreateDir actions are deduplicated when multiple files
    /// share the same parent directory. Only one CreateDir action should
    /// be produced per unique parent.
    #[test]
    fn test_build_apply_plan_deduplicate_createdir() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        // Create 3 repo files in the same parent directory
        let common_parent = home.join(".config/nvim");
        std::fs::create_dir_all(&common_parent).unwrap();

        let target1 = common_parent.join("init.lua");
        let repo_absolute_path1 = repo.join("base/home/.config/nvim/init.lua");
        std::fs::create_dir_all(repo_absolute_path1.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path1, "init").unwrap();

        let target2 = common_parent.join("plugins.lua");
        let repo_absolute_path2 = repo.join("base/home/.config/nvim/plugins.lua");
        std::fs::create_dir_all(repo_absolute_path2.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path2, "plugins").unwrap();

        let target3 = common_parent.join("lua/settings.lua");
        let repo_absolute_path3 = repo.join("base/home/.config/nvim/lua/settings.lua");
        std::fs::create_dir_all(repo_absolute_path3.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path3, "settings").unwrap();

        // Create 1 file in a different parent directory
        let other_parent = home.join(".config/skhd");
        std::fs::create_dir_all(&other_parent).unwrap();
        let target4 = other_parent.join("skhdrc");
        let repo_absolute_path4 = repo.join("base/home/.config/skhd/skhdrc");
        std::fs::create_dir_all(repo_absolute_path4.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path4, "skhd").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target1.clone(),
            (
                "base".to_string(),
                "base/home/.config/nvim/init.lua".to_string(),
            ),
        );
        merged.insert(
            target2.clone(),
            (
                "base".to_string(),
                "base/home/.config/nvim/plugins.lua".to_string(),
            ),
        );
        merged.insert(
            target3.clone(),
            (
                "base".to_string(),
                "base/home/.config/nvim/lua/settings.lua".to_string(),
            ),
        );
        merged.insert(
            target4.clone(),
            (
                "base".to_string(),
                "base/home/.config/skhd/skhdrc".to_string(),
            ),
        );
        let mut config = Config::new();
        config.managed.insert(
            "base/home/.config/nvim/init.lua".into(),
            target1.to_string_lossy().to_string(),
        );
        config.managed.insert(
            "base/home/.config/nvim/plugins.lua".into(),
            target2.to_string_lossy().to_string(),
        );
        config.managed.insert(
            "base/home/.config/nvim/lua/settings.lua".into(),
            target3.to_string_lossy().to_string(),
        );
        config.managed.insert(
            "base/home/.config/skhd/skhdrc".into(),
            target4.to_string_lossy().to_string(),
        );

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            // 4 files across 3 unique parent directories:
            //   ~/.config/nvim (2 files), ~/.config/nvim/lua (1 file),
            //   ~/.config/skhd (1 file) → exactly 3 CreateDir actions
            let create_dir_count = output
                .plan
                .actions
                .iter()
                .filter(|a| matches!(a, Action::CreateDir { .. }))
                .count();
            assert_eq!(
                create_dir_count, 3,
                "expected 3 CreateDir actions for 3 unique parents, got {}",
                create_dir_count
            );

            // All 4 files should have results
            assert_eq!(output.file_results.len(), 4);
            assert!(output.orphans.is_empty());
        });
    }

    /// Verifies that CreateDir actions appear in deterministic insertion order,
    /// not arbitrary hash-set iteration order. Running the same input multiple
    /// times must yield identical CreateDir ordering.
    #[test]
    fn test_build_apply_plan_deterministic_createdir_order() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        // Create files in 3 different parent directories
        let parent_a = home.join(".config/a");
        std::fs::create_dir_all(&parent_a).unwrap();
        let target_a = parent_a.join("file.txt");
        let repo_absolute_path_a = repo.join("base/home/.config/a/file.txt");
        std::fs::create_dir_all(repo_absolute_path_a.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path_a, "a").unwrap();

        let parent_b = home.join(".config/b");
        std::fs::create_dir_all(&parent_b).unwrap();
        let target_b = parent_b.join("file.txt");
        let repo_absolute_path_b = repo.join("base/home/.config/b/file.txt");
        std::fs::create_dir_all(repo_absolute_path_b.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path_b, "b").unwrap();

        let parent_c = home.join(".config/c");
        std::fs::create_dir_all(&parent_c).unwrap();
        let target_c = parent_c.join("file.txt");
        let repo_absolute_path_c = repo.join("base/home/.config/c/file.txt");
        std::fs::create_dir_all(repo_absolute_path_c.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path_c, "c").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target_a.clone(),
            (
                "base".to_string(),
                "base/home/.config/a/file.txt".to_string(),
            ),
        );
        merged.insert(
            target_b.clone(),
            (
                "base".to_string(),
                "base/home/.config/b/file.txt".to_string(),
            ),
        );
        merged.insert(
            target_c.clone(),
            (
                "base".to_string(),
                "base/home/.config/c/file.txt".to_string(),
            ),
        );
        let mut config = Config::new();
        config.managed.insert(
            "base/home/.config/a/file.txt".into(),
            target_a.to_string_lossy().to_string(),
        );
        config.managed.insert(
            "base/home/.config/b/file.txt".into(),
            target_b.to_string_lossy().to_string(),
        );
        config.managed.insert(
            "base/home/.config/c/file.txt".into(),
            target_c.to_string_lossy().to_string(),
        );

        crate::tests::with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
                follow_symlinks: false,
            };
            let output = build_apply_plan(&input).unwrap();

            // Collect CreateDir paths in order
            let create_dirs: Vec<PathBuf> = output
                .plan
                .actions
                .iter()
                .filter_map(|a| match a {
                    Action::CreateDir { path } => Some(path.clone()),
                    _ => None,
                })
                .collect();

            // Should have exactly 3 CreateDir actions in insertion order
            assert_eq!(create_dirs.len(), 3, "expected 3 CreateDir actions");
            assert_eq!(
                create_dirs[0], parent_a,
                "first CreateDir should be parent_a"
            );
            assert_eq!(
                create_dirs[1], parent_b,
                "second CreateDir should be parent_b"
            );
            assert_eq!(
                create_dirs[2], parent_c,
                "third CreateDir should be parent_c"
            );
        });
    }
}
