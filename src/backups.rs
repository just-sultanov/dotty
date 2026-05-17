use std::fs;
use std::path::Path;

/// Generate a timestamp string for backup directories.
///
/// Format: `YYYY-MM-DDTHH-MM-SS-NNN` (e.g. `2024-01-15T10-30-00-847`).
/// The trailing 3-digit millisecond component prevents collisions when
/// two runs happen within the same second.
pub fn backup_timestamp() -> String {
    let now = chrono::Local::now();
    now.format("%Y-%m-%dT%H-%M-%S-%3f").to_string()
}

/// List backup directory names sorted by name (chronological order).
pub fn list_backups(state_path: &Path) -> Vec<String> {
    let backup_dir = state_path.join("backups");

    if !backup_dir.is_dir() {
        return Vec::new();
    }

    let mut backups = Vec::new();

    for entry in fs::read_dir(&backup_dir)
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
    // Basic validation: YYYY-MM-DD
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || parts[0].parse::<u32>().is_err()
        || parts[1].parse::<u32>().is_err()
        || parts[2].parse::<u32>().is_err()
    {
        return None;
    }
    Some(format!("{}T", date))
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
        let path = dir.path().to_path_buf();
        let backup_dir = path.join("backups");
        fs::create_dir_all(&backup_dir).unwrap();
        fs::create_dir_all(backup_dir.join("2024-01-15T10-30-00")).unwrap();
        fs::create_dir_all(backup_dir.join("2024-01-16T09-15-00")).unwrap();

        let backups = list_backups(&path);
        assert_eq!(backups.len(), 2);
        assert_eq!(backups[0], "2024-01-15T10-30-00");
        assert_eq!(backups[1], "2024-01-16T09-15-00");
    }
}
