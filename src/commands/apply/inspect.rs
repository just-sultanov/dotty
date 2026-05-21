use std::fs;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::symlink::{is_symlink, would_be_circular};

/// The state of a target path on disk.
#[derive(PartialEq, Debug)]
pub(crate) enum TargetState {
    /// Symlink exists and points to the correct repo file.
    Correct,
    /// Target doesn't exist or is a wrong symlink — needs a new symlink.
    NeedsSymlink,
    /// Target is a regular file — needs backup before symlink replacement.
    NeedsBackup,
    /// Target is an existing directory — needs backup before symlink replacement.
    /// The `String` contains the absolute path of the directory to be replaced.
    NeedsBackupDir(String),
    /// Existing symlink is circular (externally created) — must be removed first.
    CircularSymlink,
}

/// Inspect the target path and determine what action is needed.
///
/// Returns `NeedsBackupDir` when the target is an existing directory, because
/// replacing a directory with a symlink requires `fs::remove_dir_all` which
/// silently destroys all contained files. This is especially critical on Windows
/// where directory-to-junction replacement is a common workflow.
pub(crate) fn inspect_target(target: &Path, expected_repo_file: &Path) -> TargetState {
    if is_symlink(target) {
        match fs::read_link(target) {
            Ok(link_target) => {
                // Canonicalize both paths before comparison to handle:
                // - `..` components (e.g., `/home/user/../user/.dotty` vs `/home/user/.dotty`)
                // - Intermediate symlinks in path components
                // - Different representations of the same path (e.g., `~/.dotty` vs
                //   `/home/user/.dotty`)
                //
                // Safe default: when canonicalization fails entirely, return NeedsSymlink.
                // Raw string comparison is fragile (e.g., `~/.dotty/base/home/.vimrc` vs
                // `/home/user/.dotty/base/home/.vimrc` won't match even though they
                // refer to the same file). A false negative (thinking a symlink is
                // incorrect when it might be correct) is safer than a false positive
                // (thinking a symlink is correct when it isn't), because the former
                // causes an unnecessary re-symlink while the latter leaves the system
                // in an incorrect state.
                let is_correct = match (
                    canonicalize_path(&link_target),
                    canonicalize_path(expected_repo_file),
                ) {
                    (Some(canonical_link), Some(canonical_expected)) => {
                        canonical_link == canonical_expected
                    }
                    (Some(canonical), None) => {
                        // Partial canonicalization: one path resolved, the other didn't.
                        // Use the canonicalized form but cannot confirm correctness.
                        debug!(
                            "canonicalization failed for path {}, using canonical form {} for comparison",
                            expected_repo_file.display(),
                            canonical.display()
                        );
                        // Cannot determine correctness without both sides canonicalized.
                        // Default to NeedsSymlink (safe default).
                        false
                    }
                    (None, _) => {
                        // Canonicalization failed entirely — cannot reliably compare paths.
                        // Default to NeedsSymlink (safe default) to avoid false positives.
                        debug!(
                            "canonicalization failed for link target {}; defaulting to NeedsSymlink",
                            link_target.display()
                        );
                        false
                    }
                };
                if is_correct {
                    return TargetState::Correct;
                }
                // Check if the existing symlink is circular (externally created cycle).
                // would_be_circular(link_target, target) returns true if following the
                // chain from link_target eventually leads back to target itself.
                if would_be_circular(&link_target, target) {
                    return TargetState::CircularSymlink;
                }
            }
            Err(_) => {
                // Can't read the link — treat as needing replacement
            }
        }
        TargetState::NeedsSymlink
    } else if target.is_dir() {
        // Check directory before generic `exists` to avoid silent destruction.
        // Directories require explicit backup before replacement with a symlink.
        TargetState::NeedsBackupDir(target.to_string_lossy().to_string())
    } else if target.exists() {
        TargetState::NeedsBackup
    } else {
        TargetState::NeedsSymlink
    }
}

/// Canonicalize a path for comparison purposes.
///
/// For paths that exist, uses `fs::canonicalize` directly.
/// For paths that may not exist yet (e.g., repo files), canonicalizes
/// the parent directory and rejoins the filename.
/// Returns `None` if canonicalization fails (e.g., parent doesn't exist).
pub(crate) fn canonicalize_path(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        fs::canonicalize(path).ok()
    } else {
        // Path doesn't exist — canonicalize parent and rejoin filename
        let parent = path.parent()?;
        let filename = path.file_name()?;
        let canonical_parent = fs::canonicalize(parent).ok()?;
        Some(canonical_parent.join(filename))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::symlink::create_symlink;

    /// Helper: create a temp dir with cleanup on drop.
    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("dotty-test-{}-", prefix));
        dir.push(&format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_canonicalize_path_existing_file() {
        let dir = temp_dir("canonical-existing");
        let file = dir.join("test.txt");
        fs::write(&file, "data").unwrap();

        let result = canonicalize_path(&file);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), file.canonicalize().unwrap());
    }

    #[test]
    fn test_canonicalize_path_nonexistent_file() {
        let dir = temp_dir("canonical-nonexistent");
        // Create the parent directory so canonicalization can resolve it,
        // but leave the file itself nonexistent.
        let parent = dir.join("subdir");
        fs::create_dir_all(&parent).unwrap();
        let file = parent.join("test.txt");

        let result = canonicalize_path(&file);
        assert!(result.is_some());
        // Should canonicalize the parent and rejoin the filename
        let expected = dir.canonicalize().unwrap().join("subdir").join("test.txt");
        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_canonicalize_path_parent_does_not_exist() {
        // Parent doesn't exist and can't be canonicalized
        let path = PathBuf::from("/this/parent/does/not/exist/file.txt");
        let result = canonicalize_path(&path);
        assert!(result.is_none());
    }

    #[test]
    fn test_inspect_target_needs_symlink_when_canonicalization_fails() {
        // Create a temp dir and a symlink pointing to a non-existent path.
        // canonicalize_path will fail for the link target, so we should get
        // NeedsSymlink (safe default) rather than a false match.
        let dir = temp_dir("inspect-canonicalize-fail");
        let target = dir.join(".config");
        let expected_repo = PathBuf::from("/nonexistent/repo/path/.config");

        // Create a symlink pointing to the non-existent path
        std::fs::remove_file(&target).ok();
        create_symlink(&expected_repo, &target).unwrap();
        assert!(target.is_symlink());

        // Even though the link target doesn't exist, canonicalization fails.
        // The safe default is NeedsSymlink, not a false "Correct" match.
        let state = inspect_target(&target, &expected_repo);
        assert_eq!(state, TargetState::NeedsSymlink);
    }

    #[test]
    fn test_inspect_target_partial_canonicalization() {
        // One path canonicalizes (the link target exists), the other doesn't
        // (expected_repo_file is in a non-existent directory).
        let dir = temp_dir("inspect-partial-canon");
        let target = dir.join(".config");
        let link_target = dir.join("actual_config");
        let expected_repo = PathBuf::from("/nonexistent/repo/.config");

        // Create the actual file that the symlink points to
        fs::write(&link_target, "data").unwrap();

        // Create symlink: target -> link_target (which exists)
        std::fs::remove_file(&target).ok();
        create_symlink(&link_target, &target).unwrap();

        // canonicalize_path(link_target) succeeds, but canonicalize_path(expected_repo) fails
        // because /nonexistent/repo doesn't exist.
        let state = inspect_target(&target, &expected_repo);
        // Partial canonicalization should default to NeedsSymlink (safe default)
        assert_eq!(state, TargetState::NeedsSymlink);
    }

    #[test]
    fn test_inspect_target_correct_symlink_with_canonicalization() {
        // Normal case: symlink points to correct repo file, both canonicalize.
        let dir = temp_dir("inspect-correct");
        let target = dir.join(".config");
        let expected_repo = dir.join("repo").join(".config");

        fs::create_dir_all(expected_repo.parent().unwrap()).unwrap();
        fs::write(&expected_repo, "config data").unwrap();

        // Create symlink: target -> expected_repo
        std::fs::remove_file(&target).ok();
        create_symlink(&expected_repo, &target).unwrap();

        let state = inspect_target(&target, &expected_repo);
        assert_eq!(state, TargetState::Correct);
    }

    #[test]
    fn test_inspect_target_correct_symlink_via_parent_canonicalization() {
        // Repo file doesn't exist yet, but its parent directory does.
        // canonicalize_path should canonicalize the parent and rejoin the filename.
        let dir = temp_dir("inspect-parent-canon");
        let target = dir.join(".config");
        let repo_parent = dir.join("repo");
        let expected_repo = repo_parent.join(".config");

        fs::create_dir_all(&repo_parent).unwrap();

        // Create symlink: target -> expected_repo (which doesn't exist yet)
        std::fs::remove_file(&target).ok();
        create_symlink(&expected_repo, &target).unwrap();

        // canonicalize_path(expected_repo) will canonicalize repo_parent and rejoin .config
        let state = inspect_target(&target, &expected_repo);
        assert_eq!(state, TargetState::Correct);
    }

    #[test]
    fn test_inspect_target_needs_backup() {
        let dir = temp_dir("inspect-backup");
        let target = dir.join(".config");
        let expected_repo = dir.join("repo").join(".config");

        fs::create_dir_all(expected_repo.parent().unwrap()).unwrap();
        fs::write(&expected_repo, "repo data").unwrap();

        // Regular file at target (not a symlink)
        fs::write(&target, "user data").unwrap();

        let state = inspect_target(&target, &expected_repo);
        assert_eq!(state, TargetState::NeedsBackup);
    }

    #[test]
    fn test_inspect_target_needs_symlink_no_target() {
        let dir = temp_dir("inspect-no-target");
        let target = dir.join(".config");
        let expected_repo = dir.join("repo").join(".config");

        fs::create_dir_all(expected_repo.parent().unwrap()).unwrap();
        fs::write(&expected_repo, "repo data").unwrap();

        // Target doesn't exist
        assert!(!target.exists());

        let state = inspect_target(&target, &expected_repo);
        assert_eq!(state, TargetState::NeedsSymlink);
    }
}
