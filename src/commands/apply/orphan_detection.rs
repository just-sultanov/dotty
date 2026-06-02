//! Orphan detection and removal for the `apply` command.
//!
//! Orphans are files listed in the merged tier map but no longer present in
//! `config.managed` — typically because they were removed from the dotty
//! repository. This module detects such orphans and builds removal actions
//! into the apply plan.
//!
//! The detection uses `config.managed` keys as the source of truth for
//! currently tracked files, ensuring consistent key format (repo_relative_path
//! strings) across both sources and avoiding mismatches from tuple
//! extraction in `merged.values()`.

use indexmap::IndexMap;
use std::collections::HashSet;
use std::path::PathBuf;
use tracing::warn;

use crate::config::Config;
use crate::paths::expand_tilde;
use crate::plan::Action;

/// Input data required for orphan detection.
pub(crate) struct OrphanDetectionInput<'a> {
    pub merged: &'a IndexMap<PathBuf, (String, String)>,
    pub config: &'a Config,
}

/// Output of orphan detection.
pub(crate) struct OrphanDetectionOutput {
    /// Detected orphans as (repo_relative_path, target_path_string) pairs.
    pub orphans: Vec<(String, String)>,
    /// Removal actions to add to the apply plan.
    pub removal_actions: Vec<Action>,
}

/// Detect orphan managed entries and produce removal actions.
///
/// Orphans are files whose `repo_relative_path` appears in `merged` but not in
/// `config.managed`. For each orphan, the function determines the correct
/// removal action (RemoveSymlink, RemoveFile) based on the file type on disk.
pub(crate) fn detect_orphans_and_build_removals(
    input: &OrphanDetectionInput,
) -> OrphanDetectionOutput {
    // Build tracked_set from config.managed keys to ensure consistent
    // key format (repo_relative_path strings) across both sources.
    let tracked_set: HashSet<&String> = input.config.managed.keys().collect();
    let mut orphans: Vec<(String, String)> = Vec::new();

    for (_target_path, (_tier, repo_relative_path)) in input.merged {
        if !tracked_set.contains(repo_relative_path) {
            orphans.push((
                repo_relative_path.clone(),
                _target_path.to_string_lossy().to_string(),
            ));
        }
    }

    // Build removal actions for each orphan target.
    let mut removal_actions: Vec<Action> = Vec::new();
    for (_repo_relative_path, target_rel) in &orphans {
        let target = match expand_tilde(target_rel) {
            Ok(t) => t,
            Err(e) => {
                warn!("cannot expand tilde for orphan {}: {}", target_rel, e);
                continue;
            }
        };

        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Target already gone — nothing to remove.
                continue;
            }
            Err(e) => {
                warn!(
                    "cannot determine type of orphan target {}: {}",
                    target.display(),
                    e
                );
                continue;
            }
        };

        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            removal_actions.push(Action::RemoveSymlink { path: target });
        } else if file_type.is_file() {
            removal_actions.push(Action::RemoveFile { path: target });
        } else if file_type.is_dir() {
            // Mitigation: directories are removed via RemoveFile which
            // will fail gracefully if the action doesn't support dirs.
            removal_actions.push(Action::RemoveFile { path: target });
        } else {
            // Special files (sockets, fifos, etc.) — best-effort RemoveFile.
            removal_actions.push(Action::RemoveFile { path: target });
        }
    }

    OrphanDetectionOutput {
        orphans,
        removal_actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::symlink::create_symlink;
    #[test]
    fn test_detect_orphans_no_orphans() {
        let mut merged = IndexMap::new();
        let target = PathBuf::from("/home/user/.vimrc");
        merged.insert(target, ("base".to_string(), "base/home/.vimrc".to_string()));

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "/home/user/.vimrc".into());

        let input = OrphanDetectionInput {
            merged: &merged,
            config: &config,
        };
        let output = detect_orphans_and_build_removals(&input);

        assert!(output.orphans.is_empty());
        assert!(output.removal_actions.is_empty());
    }

    #[test]
    fn test_detect_orphans_single_orphan() {
        let mut merged = IndexMap::new();
        let target1 = PathBuf::from("/home/user/.vimrc");
        let target2 = PathBuf::from("/home/user/.old");
        merged.insert(
            target1,
            ("base".to_string(), "base/home/.vimrc".to_string()),
        );
        merged.insert(target2, ("base".to_string(), "base/home/.old".to_string()));

        let mut config = Config::new();
        config
            .managed
            .insert("base/home/.vimrc".into(), "/home/user/.vimrc".into());
        // .old is NOT in managed → orphan

        let input = OrphanDetectionInput {
            merged: &merged,
            config: &config,
        };
        let output = detect_orphans_and_build_removals(&input);

        assert_eq!(output.orphans.len(), 1);
        assert_eq!(output.orphans[0].0, "base/home/.old");
        assert!(output.removal_actions.is_empty()); // target doesn't exist on disk
    }

    #[test]
    fn test_detect_orphan_symlink_removal() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        let target = home.join(".old_symlink");
        let repo_absolute_path = PathBuf::from("/tmp/repo/.old");
        std::fs::create_dir_all(repo_absolute_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_absolute_path, "old content").unwrap();
        create_symlink(&repo_absolute_path, &target).unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            home.join(".old_symlink"),
            ("base".to_string(), "base/home/.old_symlink".to_string()),
        );

        let config = Config::new();

        let input = OrphanDetectionInput {
            merged: &merged,
            config: &config,
        };
        let output = detect_orphans_and_build_removals(&input);

        assert_eq!(output.orphans.len(), 1);
        assert_eq!(output.removal_actions.len(), 1);
        assert!(
            matches!(&output.removal_actions[0], Action::RemoveSymlink { path } if path == &target),
            "expected RemoveSymlink for orphan symlink"
        );
    }

    #[test]
    fn test_detect_orphan_regular_file_removal() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        let target = home.join(".old_file");
        std::fs::write(&target, "stale content").unwrap();

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.old_file".to_string()),
        );

        let config = Config::new();

        let input = OrphanDetectionInput {
            merged: &merged,
            config: &config,
        };
        let output = detect_orphans_and_build_removals(&input);

        assert_eq!(output.orphans.len(), 1);
        assert_eq!(output.removal_actions.len(), 1);
        assert!(
            matches!(&output.removal_actions[0], Action::RemoveFile { path } if path == &target),
            "expected RemoveFile for orphan regular file"
        );
    }

    #[test]
    fn test_detect_orphan_non_existent_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        let target = home.join(".already_gone");
        // Do NOT create the target

        let mut merged = IndexMap::new();
        merged.insert(
            target.clone(),
            ("base".to_string(), "base/home/.already_gone".to_string()),
        );

        let config = Config::new();

        let input = OrphanDetectionInput {
            merged: &merged,
            config: &config,
        };
        let output = detect_orphans_and_build_removals(&input);

        assert_eq!(output.orphans.len(), 1);
        assert!(
            output.removal_actions.is_empty(),
            "no removal actions for non-existent orphan"
        );
    }
}
