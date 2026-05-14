use std::fs;

use anyhow::Result;
use tracing::warn;

use crate::convention::{date_to_backup_prefix, list_backups, resolve_state_path};
use crate::prompt::prompt_confirm;

/// Run the `clean` command.
pub fn run(keep: Option<usize>, before: Option<String>) -> Result<()> {
    let state_path = resolve_state_path()?;
    let backup_dir = state_path.join("backups");

    if !backup_dir.is_dir() {
        println!("No backups found.");
        return Ok(());
    }

    let all_backups = list_backups(&state_path);

    if all_backups.is_empty() {
        println!("No backups to clean.");
        return Ok(());
    }

    // Determine which backups to remove
    let mut to_remove: Vec<String> = Vec::new();

    if let Some(count) = keep {
        // Keep the N most recent backups
        if count >= all_backups.len() {
            println!(
                "Keeping all {} backups (keep count >= total).",
                all_backups.len()
            );
            return Ok(());
        }
        let num_to_remove = all_backups.len() - count;
        // Backups are sorted chronologically, so remove the oldest
        to_remove = all_backups[..num_to_remove].to_vec();
    } else if let Some(ref date_str) = before {
        // Remove backups older than the given date
        if let Some(prefix) = date_to_backup_prefix(date_str) {
            for backup in &all_backups {
                if backup < &prefix {
                    to_remove.push(backup.clone());
                }
            }
        } else {
            anyhow::bail!("Invalid date format: {}. Use YYYY-MM-DD.", date_str);
        }
    } else {
        // No filters — offer to remove all
        to_remove = all_backups.clone();
    }

    if to_remove.is_empty() {
        println!("No backups to remove.");
        return Ok(());
    }

    // Interactive confirmation for each backup
    let mut removed_count = 0usize;

    for backup in &to_remove {
        let ok = prompt_confirm(&format!("Remove backup '{}'", backup))?;
        if ok {
            let backup_path = backup_dir.join(backup);
            if let Err(e) = fs::remove_dir_all(&backup_path) {
                warn!("failed to remove '{}': {}", backup, e);
            } else {
                removed_count += 1;
            }
        }
    }

    println!(
        "Removed {} of {} backup(s).",
        removed_count,
        to_remove.len()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dir = std::env::temp_dir().join(format!("dotty_clean_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let backups = list_backups(&dir);
        assert!(backups.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_list_backups_with_entries() {
        let dir = std::env::temp_dir().join(format!("dotty_clean_test2_{}", std::process::id()));
        let backup_dir = dir.join("backups");
        fs::create_dir_all(&backup_dir).unwrap();
        fs::create_dir_all(backup_dir.join("2024-01-15T10-30-00")).unwrap();
        fs::create_dir_all(backup_dir.join("2024-01-16T09-15-00")).unwrap();

        let backups = list_backups(&dir);
        assert_eq!(backups.len(), 2);
        assert_eq!(backups[0], "2024-01-15T10-30-00");
        assert_eq!(backups[1], "2024-01-16T09-15-00");

        fs::remove_dir_all(&dir).unwrap();
    }
}
