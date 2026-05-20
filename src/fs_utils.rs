use std::fs;
use std::path::{Path, PathBuf};

use crate::error::DottyError;

/// Maximum recursion depth for directory traversal.
/// Prevents stack overflow and excessive I/O on deeply nested or symlink-heavy trees.
const MAX_WALK_DEPTH: u32 = 50;

/// Recursively walk a directory and collect all file paths.
///
/// Traversal stops at [MAX_WALK_DEPTH] levels. Symlinked directories are skipped
/// to avoid following symlinks into arbitrary locations (symlinked *files* are still collected).
pub fn walk_dir(dir: &Path, files: &mut Vec<PathBuf>, depth: u32) -> Result<(), DottyError> {
    if depth > MAX_WALK_DEPTH {
        return Ok(());
    }

    for dir_entry in fs::read_dir(dir)? {
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
            walk_dir(&path, files, depth + 1)?;
        }
    }
    Ok(())
}

/// Calculate the total size of a directory recursively in bytes.
pub fn calculate_dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Ok(meta) = fs::metadata(&path) {
                total += meta.len();
            }
        } else if path.is_dir() {
            total += calculate_dir_size(&path);
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
        walk_dir(&path, &mut files, 0).unwrap();
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
        walk_dir(&path, &mut files, 0).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_walk_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        let mut files = Vec::new();
        walk_dir(&path, &mut files, 0).unwrap();
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
        walk_dir(dir.path(), &mut files, 0).unwrap();

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
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_dir, &link_dir).unwrap();

        let mut files = Vec::new();
        walk_dir(&scan_dir, &mut files, 0).unwrap();

        // The file inside the symlinked directory should NOT be collected
        assert!(
            !files.iter().any(|f| f.file_name().unwrap() == "inside.txt"),
            "walk_dir should skip symlinked directories"
        );
    }

    #[test]
    fn test_walk_dir_collects_symlinked_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // Create a real file
        fs::write(path.join("real.txt"), "real").unwrap();

        // Create a symlink to that file
        let link_file = path.join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(path.join("real.txt"), &link_file).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(path.join("real.txt"), &link_file).unwrap();

        let mut files = Vec::new();
        walk_dir(&path, &mut files, 0).unwrap();

        // Both the real file and the symlinked file should be collected
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.file_name().unwrap() == "real.txt"));
        assert!(files.iter().any(|f| f.file_name().unwrap() == "link.txt"));
    }
}
