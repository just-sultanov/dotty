use std::fs;

use anyhow::Result;
use tracing::warn;

use crate::convention::{date_to_backup_prefix, list_backups, resolve_state_path};
use crate::prompt::prompt_confirm;

/// Determine which backups to remove based on filtering criteria.
///
/// Returns `(to_remove, skip_message)` where `skip_message` is `Some` when
/// no backups should be removed (e.g. keep count >= total).
///
/// `all_backups` must be sorted chronologically (oldest first).
pub(crate) fn filter_backups(
    all_backups: &[String],
    keep: Option<usize>,
    before: Option<&str>,
) -> Result<(Vec<String>, Option<String>), String> {
    if let Some(count) = keep {
        if count >= all_backups.len() {
            return Ok((
                Vec::new(),
                Some(format!(
                    "Keeping all {} backups (keep count >= total).",
                    all_backups.len()
                )),
            ));
        }
        let num_to_remove = all_backups.len() - count;
        // Backups are sorted chronologically, so remove the oldest
        return Ok((all_backups[..num_to_remove].to_vec(), None));
    }

    if let Some(date_str) = before {
        if let Some(prefix) = date_to_backup_prefix(date_str) {
            let to_remove: Vec<String> = all_backups
                .iter()
                .filter(|b| b.as_str() < prefix.as_str())
                .cloned()
                .collect();
            return Ok((to_remove, None));
        } else {
            return Err(format!(
                "Invalid date format: {}. Use YYYY-MM-DD.",
                date_str
            ));
        }
    }

    // No filters — remove all
    Ok((all_backups.to_vec(), None))
}

/// Run the `clean` command.
pub fn run(keep: Option<usize>, before: Option<String>, yes: bool) -> Result<()> {
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
    let (to_remove, skip_message) =
        filter_backups(&all_backups, keep, before.as_deref()).map_err(|e| anyhow::anyhow!(e))?;

    if let Some(msg) = skip_message {
        println!("{}", msg);
        return Ok(());
    }

    if to_remove.is_empty() {
        println!("No backups to remove.");
        return Ok(());
    }

    // Interactive confirmation for each backup (skipped with --yes)
    let mut removed_count = 0usize;

    for backup in &to_remove {
        if yes || prompt_confirm(&format!("Remove backup '{}'", backup))? {
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

    // Tests for backup utilities (date_to_backup_prefix, list_backups) live in backups.rs.
    // This module's integration-level tests live in tests/test_remove_status_clean.rs.

    #[test]
    fn test_filter_backups_keep_n() {
        let backups = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-11T10-00-00-000".to_string(),
            "2024-01-12T10-00-00-000".to_string(),
            "2024-01-13T10-00-00-000".to_string(),
            "2024-01-14T10-00-00-000".to_string(),
        ];
        let (to_remove, skip) = filter_backups(&backups, Some(2), None).unwrap();
        assert!(skip.is_none());
        assert_eq!(to_remove.len(), 3);
        assert_eq!(to_remove[0], "2024-01-10T10-00-00-000");
        assert_eq!(to_remove[1], "2024-01-11T10-00-00-000");
        assert_eq!(to_remove[2], "2024-01-12T10-00-00-000");
    }

    #[test]
    fn test_filter_backups_keep_all() {
        let backups = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-11T10-00-00-000".to_string(),
        ];
        let (to_remove, skip) = filter_backups(&backups, Some(5), None).unwrap();
        assert!(to_remove.is_empty());
        assert!(skip.is_some());
        assert!(skip.unwrap().contains("Keeping all 2 backups"));
    }

    #[test]
    fn test_filter_backups_before_date() {
        let backups = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-12T10-00-00-000".to_string(),
            "2024-01-15T10-00-00-000".to_string(),
            "2024-01-20T10-00-00-000".to_string(),
        ];
        let (to_remove, skip) = filter_backups(&backups, None, Some("2024-01-15")).unwrap();
        assert!(skip.is_none());
        assert_eq!(to_remove.len(), 2);
        assert_eq!(to_remove[0], "2024-01-10T10-00-00-000");
        assert_eq!(to_remove[1], "2024-01-12T10-00-00-000");
    }

    #[test]
    fn test_filter_backups_no_filters_removes_all() {
        let backups = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-11T10-00-00-000".to_string(),
        ];
        let (to_remove, skip) = filter_backups(&backups, None, None).unwrap();
        assert!(skip.is_none());
        assert_eq!(to_remove.len(), 2);
    }

    #[test]
    fn test_filter_backups_invalid_date() {
        let backups = vec!["2024-01-10T10-00-00-000".to_string()];
        let result = filter_backups(&backups, None, Some("not-a-date"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid date format"));
    }
}
