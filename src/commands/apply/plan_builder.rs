use indexmap::IndexMap;
use std::path::PathBuf;

use anyhow::Result;
use tracing::warn;

use crate::config::Config;
use crate::paths::expand_tilde;
use crate::plan::{Action, Plan};

use super::inspect::{TargetState, inspect_target};

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
}

/// Output of `build_apply_plan`.
pub(crate) struct ApplyPlanOutput {
    pub plan: Plan,
    pub file_results: Vec<FileResult>,
    pub orphans: Vec<(String, String)>,
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
pub(crate) fn build_apply_plan(input: &ApplyPlanInput) -> Result<ApplyPlanOutput> {
    let mut plan = Plan::new(&input.repo_path);
    let mut file_results: Vec<FileResult> = Vec::new();

    // Process each merged file
    for (target_path, (tier, repo_rel)) in &input.merged {
        let repo_file = input.repo_path.join(repo_rel);
        let target = target_path.to_path_buf();

        // Compute overrides early so they can be used in any branch
        let overrides = input.override_map.get(target_path).cloned();

        // Check target state
        let state = match inspect_target(&target, &repo_file) {
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
                    plan.add(Action::CreateDir {
                        path: parent.to_path_buf(),
                    });
                }
                plan.add(Action::CreateSymlink {
                    target: repo_file.clone(),
                    link: target.clone(),
                    backup_path: None,
                });
                TargetState::CircularSymlink
            }
            TargetState::NeedsSymlink => {
                if let Some(parent) = target.parent() {
                    plan.add(Action::CreateDir {
                        path: parent.to_path_buf(),
                    });
                }
                plan.add(Action::CreateSymlink {
                    target: repo_file.clone(),
                    link: target.clone(),
                    backup_path: None,
                });
                TargetState::NeedsSymlink
            }
            TargetState::NeedsBackup => {
                if let Some(parent) = target.parent() {
                    plan.add(Action::CreateDir {
                        path: parent.to_path_buf(),
                    });
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
                    target: repo_file.clone(),
                    link: target.clone(),
                    backup_path: Some(backup_dest),
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
                    plan.add(Action::CreateDir {
                        path: parent.to_path_buf(),
                    });
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
                });
                plan.add(Action::CreateSymlink {
                    target: repo_file.clone(),
                    link: target.clone(),
                    backup_path: Some(backup_dest),
                });
                warn!(
                    "replacing directory {} with symlink → {}",
                    dir_path,
                    repo_file.display()
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

    // Orphan detection: managed entries not in tracked files
    let tracked_set: std::collections::HashSet<&String> =
        input.merged.values().map(|(_, r)| r as &String).collect();
    let mut orphans: Vec<(String, String)> = Vec::new();
    for (repo_rel, target_rel) in &input.config.managed {
        if !tracked_set.contains(repo_rel) {
            orphans.push((repo_rel.clone(), target_rel.clone()));
        }
    }

    // Remove orphan symlinks
    for (_repo_rel, target_rel) in &orphans {
        let target = expand_tilde(target_rel)?;
        plan.add(Action::RemoveSymlink { path: target });
    }

    Ok(ApplyPlanOutput {
        plan,
        file_results,
        orphans,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symlink::create_symlink;

    fn with_test_home<F: FnOnce(&PathBuf)>(test: F)
    where
        F: FnOnce(&PathBuf),
    {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            test(&home);
        });
    }

    #[test]
    fn test_inspect_target_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nonexistent.txt");
        let repo_file = PathBuf::from("/tmp/dotty_repo_file.txt");
        assert_eq!(
            inspect_target(&target, &repo_file),
            TargetState::NeedsSymlink
        );
    }

    #[test]
    fn test_inspect_target_circular_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("circular_link");
        let repo_file = PathBuf::from("/tmp/repo.txt");

        create_symlink(&target, &target).unwrap();
        assert!(crate::symlink::is_symlink(&target));

        assert_eq!(
            inspect_target(&target, &repo_file),
            TargetState::CircularSymlink
        );
    }

    #[test]
    fn test_inspect_target_circular_symlink_two_node() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let repo_file = PathBuf::from("/tmp/repo.txt");

        create_symlink(&b, &a).unwrap();
        create_symlink(&a, &b).unwrap();

        assert_eq!(inspect_target(&a, &repo_file), TargetState::CircularSymlink);
        assert_eq!(inspect_target(&b, &repo_file), TargetState::CircularSymlink);
    }

    #[test]
    fn test_inspect_target_regular_file() {
        with_test_home(|home| {
            let target = home.join("file.txt");
            std::fs::write(&target, "content").unwrap();
            let repo_file = PathBuf::from("/tmp/repo.txt");

            assert_eq!(
                inspect_target(&target, &repo_file),
                TargetState::NeedsBackup
            );
        });
    }

    #[test]
    fn test_inspect_target_directory() {
        with_test_home(|home| {
            let target = home.join("config_dir");
            std::fs::create_dir(&target).unwrap();
            let repo_file = PathBuf::from("/tmp/repo.txt");

            let state = inspect_target(&target, &repo_file);
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
        let repo_file = sub.join("file.txt");
        std::fs::write(&repo_file, "content").unwrap();

        let target_dir = base.join("home");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("link");

        let link_target = base.join("repo").join("..").join("repo").join("file.txt");
        create_symlink(&link_target, &target).unwrap();

        assert_eq!(inspect_target(&target, &repo_file), TargetState::Correct);
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
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();
        create_symlink(&repo_file, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
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
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let config = Config::new();

        with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
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
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();
        create_symlink(&target, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let config = Config::new();

        with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
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
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "new content").unwrap();
        std::fs::write(&target, "old content").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let config = Config::new();

        with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
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
        let repo_file = repo.join("base/home/.config_dir");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "new content").unwrap();
        // Create a real directory at the target location
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inner.txt"), "inner").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.config_dir".to_string()),
        );
        let config = Config::new();

        with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
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
        let repo_file = repo.join("base/home/.config_dir");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "new content").unwrap();
        // Create a real directory at the target location
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inner.txt"), "inner").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.config_dir".to_string()),
        );
        let config = Config::new();

        with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: true,
            };
            let output = build_apply_plan(&input).unwrap();

            // With --force, directory replacement should proceed
            assert_eq!(output.plan.actions.len(), 3);
            assert!(
                matches!(&output.plan.actions[0], Action::CreateDir { .. }),
                "first action should be CreateDir"
            );
            assert!(
                matches!(&output.plan.actions[1], Action::BackupDir { .. }),
                "second action should be BackupDir"
            );
            assert!(
                matches!(&output.plan.actions[2], Action::CreateSymlink { .. }),
                "third action should be CreateSymlink"
            );
            assert_eq!(output.file_results.len(), 1);
            assert!(output.file_results[0].applied);
        });
    }

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
        let repo_file = repo.join("base/home/.vimrc");
        std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
        std::fs::write(&repo_file, "content").unwrap();
        create_symlink(&repo_file, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());
        config
            .managed
            .insert("base/home/.old".into(), "~/.old".into());

        with_test_home(|_| {
            let input = ApplyPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                merged,
                override_map: IndexMap::new(),
                config,
                force: false,
            };
            let output = build_apply_plan(&input).unwrap();

            assert_eq!(output.orphans.len(), 1);
            assert_eq!(output.orphans[0].0, "base/home/.old");
            assert!(!output.plan.is_empty());
        });
    }
}
