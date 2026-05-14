use std::fs;
use std::os::unix::fs::symlink as unix_symlink;
use std::path::Path;

/// Check if a path is a symlink (without following it).
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Create a symlink at `link` pointing to `target`.
///
/// This is a thin wrapper around `std::os::unix::fs::symlink` that returns
/// a `Result` for consistent error handling across the codebase.
pub fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    unix_symlink(target, link)
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
}
