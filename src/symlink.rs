use std::fs;
use std::path::{Path, PathBuf};

/// Maximum number of symlink hops to follow before declaring a cycle.
const MAX_SYMLINK_HOPS: usize = 40;

/// Check if a path is a symlink (without following it).
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Create a symlink at `link` pointing to `target`.
///
/// Uses `symlink_rs` for cross-platform support:
/// - On Unix, this is equivalent to `std::os::unix::fs::symlink` (no distinction
///   between file and directory symlinks).
/// - On Windows, `symlink_file` is used for file targets and `symlink_dir`
///   (junction) for directory targets. Windows requires this distinction:
///   `symlink_file` fails when the target is a directory, and `symlink_dir`
///   creates a reparse point (junction) that works for directories.
///
/// When the target does not yet exist, a file symlink is created as the default.
pub fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    // On Windows, directory symlinks require `symlink_dir` (junctions).
    // `symlink_file` silently fails or errors when the target is a directory.
    // On Unix, `symlink_file` handles both files and directories transparently,
    // so we only need the platform-specific branch for Windows.
    #[cfg(windows)]
    {
        let target_is_dir = target.is_dir();
        if target_is_dir {
            symlink_rs::symlink_dir(target, link)
        } else {
            symlink_rs::symlink_file(target, link)
        }
    }
    #[cfg(not(windows))]
    {
        symlink_rs::symlink_file(target, link)
    }
}

/// Check if creating a symlink at `link` pointing to `target` would create
/// a circular reference.
///
/// A circular symlink occurs when following the chain of symlinks from `target`
/// eventually leads back to `link` itself. This is detected by walking the
/// symlink chain up to `MAX_SYMLINK_HOPS` steps.
///
/// `..` resolution logic: when the link path exists, we canonicalize it to get
/// its true absolute path. The target path is then resolved relative to the
/// link's parent directory so that `..` components are resolved against the
/// correct location. This prevents false negatives where `../foo` in the target
/// would otherwise resolve against the current working directory instead of the
/// link's parent.
pub fn would_be_circular(target: &Path, link: &Path) -> bool {
    // Resolve the absolute path where the symlink will reside.
    let link_abs = resolve_path(link);

    // If target directly resolves to the link path, it's circular (self-reference).
    let link_parent = link_abs.parent().unwrap_or(&link_abs);
    let target_resolved = resolve_target_with_parent(target, link_parent);
    if target_resolved == link_abs {
        return true;
    }

    // Walk the symlink chain starting from `target`.
    let mut current = target_resolved;
    for _ in 0..MAX_SYMLINK_HOPS {
        // If current is a symlink, follow it
        if is_symlink(&current) {
            match fs::read_link(&current) {
                Ok(next) => {
                    // Resolve relative symlink targets against the symlink's directory
                    current = if next.is_absolute() {
                        next
                    } else {
                        current
                            .parent()
                            .map(|p| {
                                let parent = fs::canonicalize(p).unwrap_or(p.to_path_buf());
                                parent.join(&next)
                            })
                            .unwrap_or_else(|| next.clone())
                    };

                    // Check if we've looped back to the link path
                    if resolve_path(&current) == link_abs {
                        return true;
                    }
                }
                Err(_) => break, // Can't read link, stop
            }
        } else {
            // Reached a non-symlink — no cycle
            return false;
        }
    }

    // Exceeded max hops — likely a cycle
    true
}

/// Resolve a target path against a known link parent for circular detection.
///
/// Relative target paths with `..` components are resolved against the link's
/// parent directory. This ensures that `../foo` in a target resolves to the
/// correct location rather than the current working directory.
pub fn resolve_target_with_parent(target: &Path, link_parent: &Path) -> PathBuf {
    if target.is_absolute() {
        // Absolute targets are resolved independently
        resolve_path(target)
    } else {
        // Relative targets are resolved against the link's parent directory
        let resolved = link_parent.join(target);
        // Try canonicalize first (works when the resolved path exists)
        if let Ok(canonical) = fs::canonicalize(&resolved) {
            canonical
        } else {
            // Best effort: normalize using path components to resolve `..`
            let mut stack: Vec<std::path::Component<'_>> = Vec::new();
            for comp in resolved.components() {
                if comp == std::path::Component::ParentDir {
                    stack.pop();
                } else {
                    stack.push(comp);
                }
            }
            stack.iter().collect::<PathBuf>()
        }
    }
}

/// Resolve a path to its absolute form.
///
/// For existing paths, canonicalizes. For paths that don't exist yet,
/// resolves the parent directory and appends the file name.
fn resolve_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(canonical) = fs::canonicalize(path) {
        canonical
    } else {
        // Path doesn't exist — resolve parent + file name
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => {
                let parent_abs = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
                parent_abs.join(name)
            }
            _ => path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_symlink_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("regular.txt");
        fs::write(&file, "content").unwrap();

        assert!(!is_symlink(&file));
    }

    #[test]
    fn test_is_symlink_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, "content").unwrap();
        crate::symlink::create_symlink(&target, &link).unwrap();

        assert!(is_symlink(&link));
    }

    #[test]
    fn test_is_symlink_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");
        assert!(!is_symlink(&path));
    }

    #[test]
    fn test_would_be_circular_self_reference() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("self_link");
        // A symlink pointing to itself
        assert!(would_be_circular(&link, &link));
    }

    #[test]
    fn test_would_be_circular_chain() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        // Create a -> b, then check if b -> a would be circular
        create_symlink(&b, &a).unwrap();
        assert!(would_be_circular(&a, &b));
    }

    #[test]
    fn test_would_not_be_circular_normal() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real_file");
        let link = dir.path().join("link_to_file");
        fs::write(&target, "content").unwrap();

        assert!(!would_be_circular(&target, &link));
    }

    /// Verify that creating a symlink to an existing directory succeeds.
    ///
    /// On Unix, `symlink_file` handles directory targets transparently.
    /// On Windows, this test exercises the `symlink_dir` branch.
    #[test]
    fn test_create_symlink_to_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target_dir");
        let link = dir.path().join("link_to_dir");

        fs::create_dir(&target_dir).unwrap();

        create_symlink(&target_dir, &link).unwrap();
        assert!(is_symlink(&link));
        assert_eq!(fs::read_link(&link).unwrap(), target_dir);
    }

    /// Verify that replacing an existing directory with a symlink succeeds.
    ///
    /// This is the core Windows bug scenario: when the link path is already a
    /// real directory, it must be removed before the symlink can be created.
    /// On Windows, the replacement symlink must use `symlink_dir` (junction)
    /// because the target is a directory.
    #[test]
    fn test_create_symlink_replaces_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target_dir");
        let link = dir.path().join("link_to_dir");

        // Create a real directory at the link location
        fs::create_dir(&link).unwrap();
        assert!(link.is_dir());
        assert!(!is_symlink(&link));

        // Create the actual target directory
        fs::create_dir(&target_dir).unwrap();

        // Remove the existing directory and create symlink
        fs::remove_dir_all(&link).unwrap();
        create_symlink(&target_dir, &link).unwrap();

        assert!(is_symlink(&link));
        assert_eq!(fs::read_link(&link).unwrap(), target_dir);
    }

    /// Verify that a symlink with `..` resolving to itself is detected as circular.
    ///
    /// Scenario: link at `/dir/subdir/link` with target `../subdir/link`.
    /// The `..` resolves back to the same location, forming a self-reference.
    #[test]
    fn test_would_be_circular_with_dotdot_self_ref() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let link = subdir.join("link");
        // Target is relative: ../subdir/link which resolves to the same path
        let target = PathBuf::from("../subdir/link");

        assert!(would_be_circular(&target, &link));
    }

    /// Verify that a symlink with `..` resolving to a different path is allowed.
    ///
    /// Scenario: link at `/dir/subdir/link` with target `../other`. Since `other`
    /// is a different location, this should not be detected as circular.
    #[test]
    fn test_would_not_be_circular_with_dotdot_different() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        let other = dir.path().join("other");
        fs::create_dir(&subdir).unwrap();
        fs::write(&other, "content").unwrap();

        let link = subdir.join("link");
        let target = PathBuf::from("../other");

        assert!(!would_be_circular(&target, &link));
    }

    /// Verify that a symlink with `..` resolving to a valid non-circular path is allowed.
    #[test]
    fn test_would_not_be_circular_with_dotdot_valid() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let c = dir.path().join("c");
        fs::write(&b, "content").unwrap();
        fs::write(&c, "content").unwrap();

        let link = a.join("link");
        // Target: ../b resolves to dir/b, which is not the link itself
        let target = PathBuf::from("../b");

        assert!(!would_be_circular(&target, &link));
    }

    /// Verify circular detection when link exists as a real directory before symlink.
    ///
    /// This tests the pre-create check scenario where the link path doesn't
    /// exist yet but its parent does.
    #[test]
    fn test_would_be_circular_pre_create_with_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let link = subdir.join("link");
        // Link doesn't exist yet, but parent does
        // Target: ../subdir/link would resolve to the same path
        let target = PathBuf::from("../subdir/link");

        assert!(would_be_circular(&target, &link));
    }
}
