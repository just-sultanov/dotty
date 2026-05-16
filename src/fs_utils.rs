use std::fs;
use std::path::{Path, PathBuf};

use crate::error::DottyError;

/// Recursively walk a directory and collect all file paths.
pub fn walk_dir(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), DottyError> {
    for dir_entry in fs::read_dir(dir)? {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();

        // is_file() follows symlinks; symlink_metadata checks the link itself
        let is_file_or_symlink = path.is_file()
            || path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink());

        if is_file_or_symlink {
            files.push(path);
        } else if path.is_dir() {
            walk_dir(&path, files)?;
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
        let dir = std::env::temp_dir().join(format!("dotty_walk_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "content").unwrap();

        let mut files = Vec::new();
        walk_dir(&dir, &mut files).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "a.txt");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_walk_dir_nested() {
        let dir = std::env::temp_dir().join(format!("dotty_walk_test2_{}", std::process::id()));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("sub").join("b.txt"), "b").unwrap();

        let mut files = Vec::new();
        walk_dir(&dir, &mut files).unwrap();
        assert_eq!(files.len(), 2);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_walk_dir_empty() {
        let dir = std::env::temp_dir().join(format!("dotty_walk_test3_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let mut files = Vec::new();
        walk_dir(&dir, &mut files).unwrap();
        assert!(files.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_calculate_dir_size() {
        let dir = std::env::temp_dir().join(format!("dotty_size_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "12345").unwrap(); // 5 bytes
        fs::write(dir.join("b.txt"), "1234567890").unwrap(); // 10 bytes

        let size = calculate_dir_size(&dir);
        assert_eq!(size, 15);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_calculate_dir_size_empty() {
        let dir = std::env::temp_dir().join(format!("dotty_size_test2_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let size = calculate_dir_size(&dir);
        assert_eq!(size, 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_calculate_dir_size_nonexistent() {
        let size = calculate_dir_size(Path::new("/nonexistent/path/that/does/not/exist"));
        assert_eq!(size, 0);
    }
}
