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
/// Uses `symlink_rs::symlink_file` for cross-platform support:
/// on Unix this is equivalent to `std::os::unix::fs::symlink`,
/// on Windows it calls `std::os::windows::fs::symlink_file`.
pub fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    symlink_rs::symlink_file(target, link)
}

/// Check if creating a symlink at `link` pointing to `target` would create
/// a circular reference.
///
/// A circular symlink occurs when following the chain of symlinks from `target`
/// eventually leads back to `link` itself. This is detected by walking the
/// symlink chain up to `MAX_SYMLINK_HOPS` steps.
pub fn would_be_circular(target: &Path, link: &Path) -> bool {
    // Resolve the absolute path where the symlink will reside.
    let link_abs = resolve_path(link);

    // If target directly resolves to the link path, it's circular (self-reference).
    let target_resolved = resolve_path(target);
    if target_resolved == link_abs {
        return true;
    }

    // Walk the symlink chain starting from `target`.
    let mut current = target.to_path_buf();
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
    use serial_test::serial;
    use std::env::temp_dir;
    use std::path::PathBuf;

    #[test]
    #[serial]
    fn test_is_symlink_regular_file() {
        let dir = temp_dir().join(format!("dotty_symlink_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("regular.txt");
        fs::write(&file, "content").unwrap();

        assert!(!is_symlink(&file));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[serial]
    fn test_is_symlink_symlink() {
        let dir = temp_dir().join(format!("dotty_symlink_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target.txt");
        let link = dir.join("link.txt");
        fs::write(&target, "content").unwrap();
        crate::symlink::create_symlink(&target, &link).unwrap();

        assert!(is_symlink(&link));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_is_symlink_nonexistent() {
        let path = PathBuf::from(format!("/tmp/dotty_nonexistent_{}.txt", std::process::id()));
        assert!(!is_symlink(&path));
    }

    #[test]
    #[serial]
    fn test_would_be_circular_self_reference() {
        let dir = temp_dir().join(format!("dotty_circular_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let link = dir.join("self_link");
        // A symlink pointing to itself
        assert!(would_be_circular(&link, &link));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[serial]
    fn test_would_be_circular_chain() {
        let dir = temp_dir().join(format!("dotty_circular_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let a = dir.join("a");
        let b = dir.join("b");
        // Create a -> b, then check if b -> a would be circular
        create_symlink(&b, &a).unwrap();
        assert!(would_be_circular(&a, &b));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[serial]
    fn test_would_not_be_circular_normal() {
        let dir = temp_dir().join(format!("dotty_circular_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let target = dir.join("real_file");
        let link = dir.join("link_to_file");
        fs::write(&target, "content").unwrap();

        assert!(!would_be_circular(&target, &link));

        fs::remove_dir_all(&dir).unwrap();
    }
}
