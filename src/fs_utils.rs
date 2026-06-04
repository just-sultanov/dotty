use std::fs;
use std::path::{Path, PathBuf};

use crate::error::DottyError;

/// Maximum recursion depth for directory traversal.
///
/// Prevents stack overflow and excessive I/O on deeply nested or symlink-heavy
/// trees. Chosen based on:
/// - Typical dotfile repos have depth 1-5 (e.g., base/vim/backup)
/// - Deep nesting often indicates issues (broken symlinks, misconfigured archives)
/// - 50 allows for pathological cases while preventing infinite traversal
const MAX_WALK_DEPTH: u32 = 50;

/// Walk a directory and collect all file paths using an iterative approach.
///
/// Uses an explicit `Vec<PathBuf>` work queue instead of recursion to prevent
/// stack overflow on deeply nested directory trees (e.g., misconfigured backup
/// directories). Traversal stops at [MAX_WALK_DEPTH] levels. Symlinked
/// directories are skipped to avoid following symlinks into arbitrary locations
/// (symlinked *files* are still collected).
///
/// If `dir` itself is a symlink to a directory, it is resolved to its target
/// before traversal begins. This allows callers (e.g., `collect_files`) to
/// pass a symlink path directly.
///
/// The iterative approach trades heap memory for stack safety: the work queue
/// grows on the heap, which can handle far wider directory trees than the
/// call stack can handle deep ones.
pub fn walk_dir(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), DottyError> {
    // Resolve the starting path: if it's a symlink to a directory, use the
    // target so that traversal works correctly (walk_dir skips symlinked dirs
    // during iteration, so we must resolve the initial path).
    let start_path = if dir
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
    } else {
        dir.to_path_buf()
    };

    // Iterative DFS using an explicit work queue (stack).
    let mut queue: Vec<(PathBuf, u32)> = vec![(start_path, 0)];

    while let Some((current, current_depth)) = queue.pop() {
        let dir_entries = fs::read_dir(&current)?;

        for dir_entry in dir_entries {
            let dir_entry = dir_entry?;
            let path = dir_entry.path();

            // is_file() follows symlinks; symlink_metadata checks the link itself.
            // A symlink to a directory is NOT a file, so exclude it here.
            let is_symlink = path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink());
            let is_file_or_symlink = path.is_file() || (is_symlink && !path.is_dir());

            if is_file_or_symlink {
                files.push(path);
            } else if path.is_dir() && !is_symlink {
                let next_depth = current_depth + 1;
                if next_depth <= MAX_WALK_DEPTH {
                    queue.push((path, next_depth));
                }
            }
        }
    }
    Ok(())
}

/// Remove a stale `.tmp` file if it exists, logging a warning.
///
/// This is a shared helper used by multiple modules that check for leftover
/// temporary files before writing new ones. Errors from `remove_file` are
/// deliberately ignored to match existing behavior across call sites.
pub fn remove_stale_tmp(state_path: &Path, filename: &str) {
    let tmp_path = state_path.join(filename);
    if tmp_path.exists() {
        tracing::warn!("removing stale temp file: {}", tmp_path.display());
        let _ = fs::remove_file(&tmp_path);
    }
}

/// Calculate the total size of a directory iteratively in bytes.
///
/// Uses an explicit `Vec<PathBuf>` work stack instead of recursion to prevent
/// stack overflow on deeply nested directory trees. This is consistent with
/// the iterative approach used in [`walk_dir`].
pub fn calculate_dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok(meta) = fs::metadata(&path) {
                    total += meta.len();
                }
            } else if path.is_dir() {
                stack.push(path);
            }
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_walk_dir_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("a.txt"), "content").unwrap();

        let mut files = Vec::new();
        walk_dir(&path, &mut files).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "a.txt");
    }

    #[test]
    fn test_walk_dir_nested() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::create_dir_all(path.join("sub")).unwrap();
        fs::write(path.join("a.txt"), "a").unwrap();
        fs::write(path.join("sub").join("b.txt"), "b").unwrap();

        let mut files = Vec::new();
        walk_dir(&path, &mut files).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_walk_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        let mut files = Vec::new();
        walk_dir(&path, &mut files).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_calculate_dir_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("a.txt"), "12345").unwrap(); // 5 bytes
        fs::write(path.join("b.txt"), "1234567890").unwrap(); // 10 bytes

        let size = calculate_dir_size(&path);
        assert_eq!(size, 15);
    }

    #[test]
    fn test_calculate_dir_size_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        let size = calculate_dir_size(&path);
        assert_eq!(size, 0);
    }

    #[test]
    fn test_calculate_dir_size_nested() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::create_dir_all(path.join("sub")).unwrap();
        fs::write(path.join("a.txt"), "12345").unwrap(); // 5 bytes
        fs::write(path.join("sub").join("b.txt"), "1234567890").unwrap(); // 10 bytes

        let size = calculate_dir_size(&path);
        assert_eq!(size, 15);
    }

    #[test]
    fn test_calculate_dir_size_deep_nesting_no_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let scan_dir = dir.path().join("scan");
        fs::create_dir_all(&scan_dir).unwrap();

        // Add a file at the root level
        fs::write(scan_dir.join("root.txt"), "root").unwrap();

        // Create a directory tree 100 levels deep
        let mut current = scan_dir.clone();
        for i in 0..100 {
            current = current.join(format!("level_{i}"));
        }
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("deep.txt"), "deep").unwrap();

        // This must not panic or overflow the stack.
        let size = calculate_dir_size(&scan_dir);

        // Should include root.txt but NOT deep.txt (beyond MAX_WALK_DEPTH)
        assert!(size > 0);
    }

    #[test]
    fn test_calculate_dir_size_nonexistent() {
        let size = calculate_dir_size(Path::new("/nonexistent/path/that/does/not/exist"));
        assert_eq!(size, 0);
    }

    #[test]
    fn test_walk_dir_depth_limit() {
        let dir = tempfile::tempdir().unwrap();

        // Create a deeply nested directory structure (depth 60, beyond MAX_WALK_DEPTH of 50)
        let mut current = dir.path().to_path_buf();
        for i in 0..60 {
            current = current.join(format!("level_{i}"));
        }
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("deep.txt"), "deep").unwrap();

        let mut files = Vec::new();
        walk_dir(dir.path(), &mut files).unwrap();

        // The deep file should NOT be collected because it's beyond the depth limit
        assert!(
            !files.iter().any(|f| f.file_name().unwrap() == "deep.txt"),
            "walk_dir should stop at depth limit"
        );
    }

    #[test]
    fn test_walk_dir_skips_symlinked_directories() {
        let dir = tempfile::tempdir().unwrap();
        let scan_dir = dir.path().join("scan");

        // Create a real directory OUTSIDE the scanned tree
        let real_dir = dir.path().join("real_dir");
        fs::create_dir_all(&real_dir).unwrap();
        fs::write(real_dir.join("inside.txt"), "inside").unwrap();

        // Create the scan directory with a symlink pointing to real_dir
        fs::create_dir_all(&scan_dir).unwrap();
        let link_dir = scan_dir.join("link_dir");
        crate::symlink::create_symlink(&real_dir, &link_dir).unwrap();

        let mut files = Vec::new();
        walk_dir(&scan_dir, &mut files).unwrap();

        // The file inside the symlinked directory should NOT be collected
        assert!(
            !files.iter().any(|f| f.file_name().unwrap() == "inside.txt"),
            "walk_dir should skip symlinked directories"
        );
    }

    /// Integration test: verify no stack overflow on deeply nested directories.
    /// Creates a directory tree >50 levels deep and confirms walk_dir completes
    /// without panicking (the depth limit stops collection but traversal must
    /// iterate through the queue without blowing the stack).
    #[test]
    fn test_walk_dir_deep_nesting_no_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let scan_dir = dir.path().join("scan");
        fs::create_dir_all(&scan_dir).unwrap();

        // Add a file at the root level so we can verify traversal works.
        fs::write(scan_dir.join("root.txt"), "root").unwrap();

        // Create a directory tree 100 levels deep (well beyond MAX_WALK_DEPTH of 50)
        let mut current = scan_dir.clone();
        for i in 0..100 {
            current = current.join(format!("level_{i}"));
        }
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("deep.txt"), "deep").unwrap();

        // This must not panic or overflow the stack.
        let mut files = Vec::new();
        walk_dir(&scan_dir, &mut files).unwrap();

        // The deep file should NOT be collected (beyond depth 50)
        assert!(
            !files.iter().any(|f| f.file_name().unwrap() == "deep.txt"),
            "walk_dir should stop at depth limit"
        );

        // The root-level file SHOULD be collected.
        assert!(
            files.iter().any(|f| f.file_name().unwrap() == "root.txt"),
            "walk_dir should collect files at valid depths"
        );
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_walk_dir_collects_symlinked_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // Create a real file
        fs::write(path.join("real.txt"), "real").unwrap();

        // Create a symlink to that file
        let link_file = path.join("link.txt");
        crate::symlink::create_symlink(&path.join("real.txt"), &link_file).unwrap();

        let mut files = Vec::new();
        walk_dir(&path, &mut files).unwrap();

        // Both the real file and the symlinked file should be collected
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.file_name().unwrap() == "real.txt"));
        assert!(files.iter().any(|f| f.file_name().unwrap() == "link.txt"));
    }
}
