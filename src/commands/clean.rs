use std::fs;

use anyhow::Result;
use tracing::warn;

use crate::convention::{date_to_backup_prefix, list_backups, resolve_state_path};
use crate::prompt::prompt_confirm;

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
    // Tests for backup utilities (date_to_backup_prefix, list_backups) live in backups.rs.
    // This module's integration-level tests live in tests/test_remove_status_clean.rs.
}
