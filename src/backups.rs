use std::fs;
use std::path::{Path, PathBuf};

/// Construct the backup destination path for a given target file.
///
/// If `target` is under `home`, the backup preserves the relative path
/// (e.g. `~/.config/foo` → `backups/<ts>/.config/foo`).
/// Otherwise, only the file name is used (e.g. `/tmp/foo` → `backups/<ts>/foo`).
///
/// # Parameters
/// * `target` — the real-path of the file being backed up
/// * `home` — the user's home directory
/// * `backups_path` — the dotty backups directory
/// * `ts` — the backup timestamp string (from [`backup_timestamp`])
pub fn backup_dest_for(target: &Path, home: &Path, backups_path: &Path, ts: &str) -> PathBuf {
    let backup_base = backups_path.join(ts);
    if let Ok(relative) = target.strip_prefix(home) {
        backup_base.join(relative)
    } else {
        backup_base.join(target.file_name().unwrap_or_default())
    }
}

/// Generate a timestamp string for backup directories.
///
/// Format: `YYYY-MM-DDTHH-MM-SS-NNN` (e.g. `2024-01-15T10-30-00-847`).
/// The trailing 3-digit millisecond component prevents collisions when
/// two runs happen within the same second.
pub fn backup_timestamp() -> String {
    let now = chrono::Local::now();
    // Use hyphens instead of colons because colons are invalid in Windows filenames.
    now.format("%Y-%m-%dT%H-%M-%S-%3f").to_string()
}

/// List backup directory names sorted by name (chronological order).
pub fn list_backups(backups_path: &Path) -> Vec<String> {
    if !backups_path.is_dir() {
        return Vec::new();
    }

    let mut backups = Vec::new();

    for entry in fs::read_dir(backups_path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() {
            backups.push(name);
        }
    }

    backups.sort();
    backups
}

/// Parse a date string in YYYY-MM-DD format and return the corresponding
/// backup timestamp prefix for comparison.
///
/// Backup timestamps are in format YYYY-MM-DDTHH-MM-SS-NNN, so a date "2024-01-15"
/// matches all backups starting with "2024-01-15T".
pub fn date_to_backup_prefix(date: &str) -> Option<String> {
    if date.len() != 10 {
        return None;
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|d| format!("{}T", d.format("%Y-%m-%d")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_backup_timestamp_format() {
        let ts = backup_timestamp();
        assert_eq!(ts.len(), 23, "timestamp length should be 23 (with millis)");
        assert!(ts.chars().nth(4) == Some('-'), "missing dash at position 4");
        assert!(ts.chars().nth(10) == Some('T'), "missing T at position 10");
        // Last 3 chars should be digits (milliseconds), preceded by '-'
        let millis = ts.rsplit('-').next().unwrap();
        assert_eq!(millis.len(), 3, "millis should be 3 digits");
        assert!(
            millis.chars().all(|c| c.is_ascii_digit()),
            "millis should be digits"
        );
    }

    #[test]
    fn test_date_to_backup_prefix_valid() {
        let prefix = date_to_backup_prefix("2024-01-15");
        assert_eq!(prefix, Some("2024-01-15T".to_string()));
    }

    #[test]
    fn test_date_to_backup_prefix_invalid_short() {
        assert!(date_to_backup_prefix("2024-1-15").is_none());
    }

    #[test]
    fn test_date_to_backup_prefix_invalid_chars() {
        assert!(date_to_backup_prefix("abcd-ef-gh").is_none());
    }

    #[test]
    fn test_date_to_backup_prefix_wrong_length() {
        assert!(date_to_backup_prefix("2024-01-1").is_none());
    }

    #[test]
    fn test_backup_comparison() {
        // Backup timestamps are lexicographically sortable
        assert!("2024-01-15T10-30-00" < "2024-01-15T11-00-00");
        assert!("2024-01-14T23-59-59" < "2024-01-15T00-00-00");
    }

    #[test]
    fn test_list_backups_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        let backups = list_backups(&path);
        assert!(backups.is_empty());
    }

    #[test]
    fn test_list_backups_with_entries() {
        let dir = tempfile::tempdir().unwrap();
        let backups_path = dir.path().to_path_buf();
        fs::create_dir_all(backups_path.join("2024-01-15T10-30-00")).unwrap();
        fs::create_dir_all(backups_path.join("2024-01-16T09-15-00")).unwrap();

        let backups = list_backups(&backups_path);
        assert_eq!(backups.len(), 2);
        assert_eq!(backups[0], "2024-01-15T10-30-00");
        assert_eq!(backups[1], "2024-01-16T09-15-00");
    }

    #[test]
    fn test_backup_dest_for_inside_home() {
        let home = Path::new("/home/user");
        let backups_path = Path::new("/home/user/.dotty/backups");
        let target = Path::new("/home/user/.config/alacritty/alacritty.toml");
        let ts = "2024-01-15T10-30-00-847";

        let dest = backup_dest_for(target, home, backups_path, ts);

        assert_eq!(
            dest,
            PathBuf::from(
                "/home/user/.dotty/backups/2024-01-15T10-30-00-847/.config/alacritty/alacritty.toml"
            )
        );
    }

    #[test]
    fn test_backup_dest_for_outside_home() {
        let home = Path::new("/home/user");
        let backups_path = Path::new("/home/user/.dotty/backups");
        let target = Path::new("/tmp/somefile");
        let ts = "2024-01-15T10-30-00-847";

        let dest = backup_dest_for(target, home, backups_path, ts);

        assert_eq!(
            dest,
            PathBuf::from("/home/user/.dotty/backups/2024-01-15T10-30-00-847/somefile")
        );
    }

    #[test]
    fn test_backup_dest_for_root_relative() {
        let home = Path::new("/home/user");
        let backups_path = Path::new("/home/user/.dotty/backups");
        let target = Path::new("/etc/ssh/sshd_config");
        let ts = "2024-01-15T10-30-00-847";

        let dest = backup_dest_for(target, home, backups_path, ts);

        assert_eq!(
            dest,
            PathBuf::from("/home/user/.dotty/backups/2024-01-15T10-30-00-847/sshd_config")
        );
    }
}
