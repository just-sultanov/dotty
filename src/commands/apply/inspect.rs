use std::fs;
use std::path::{Path, PathBuf};

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
                // If canonicalization fails (e.g., permission denied), fall back to
                // the original string comparison.
                let is_correct = match (
                    canonicalize_path(&link_target),
                    canonicalize_path(expected_repo_file),
                ) {
                    (Some(canonical_link), Some(canonical_expected)) => {
                        canonical_link == canonical_expected
                    }
                    _ => {
                        // Fallback: compare raw paths when canonicalization is not possible
                        link_target == *expected_repo_file
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
