use anyhow::Result;

use crate::backups::{date_to_backup_prefix, list_backups};
use crate::plan::{self, Action, ExecuteMode, Plan};
use crate::repo_state::RepoState;

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
        // When both flags are present, restrict the candidate set to backups
        // before the date first, then keep the N newest among those.
        if let Some(date_str) = before {
            let prefix = date_to_backup_prefix(date_str)
                .ok_or_else(|| format!("Invalid date format: {}. Use YYYY-MM-DD.", date_str))?;
            let before_backups: Vec<&String> = all_backups
                .iter()
                .filter(|b| b.as_str() < prefix.as_str())
                .collect();
            if count >= before_backups.len() {
                return Ok((
                    Vec::new(),
                    Some(format!(
                        "Keeping all {} backups before {} (keep count >= total).",
                        before_backups.len(),
                        date_str
                    )),
                ));
            }
            let num_to_remove = before_backups.len() - count;
            let to_remove: Vec<String> = before_backups
                .iter()
                .take(num_to_remove)
                .map(|s| (*s).clone())
                .collect();
            return Ok((to_remove, None));
        }

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
        let (to_remove, _) = all_backups.split_at(num_to_remove);
        return Ok((to_remove.to_vec(), None));
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
    let mut repo = RepoState::new()?;
    let backup_dir = repo.backups_path.clone();

    if !backup_dir.is_dir() {
        println!("No backups found.");
        return Ok(());
    }

    let all_backups = list_backups(&backup_dir);

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

    // Build a plan with a single group Confirm wrapping all RemoveDir actions
    let remove_actions: Vec<Action> = to_remove
        .iter()
        .map(|b| Action::RemoveDir {
            path: backup_dir.join(b),
        })
        .collect();

    let confirm_action = Action::Confirm {
        prompt: if yes {
            None
        } else {
            Some(format!("Remove {} backup(s)?", remove_actions.len()))
        },
        actions: remove_actions,
    };

    let plan = Plan::builder(&repo.repo_path).with(confirm_action).build();
    plan::execute_plan(&plan, ExecuteMode::Normal, &mut repo)?;

    // Count actually removed (handles both confirmed and skipped cases)
    let removed_count = to_remove
        .iter()
        .filter(|b| !backup_dir.join(b).exists())
        .count();
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
    use crate::plan::action_execute;
    use std::path::PathBuf;

    // Tests for backup utilities (date_to_backup_prefix, list_backups) live in backups.rs.
    // This module's integration-level tests live in tests/test_remove_status_clean.rs.

    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn init_git_repo(path: &PathBuf) {
        std::process::Command::new("git")
            .current_dir(path)
            .args(["init"])
            .output()
            .expect("git init should work in test env");
    }

    /// Test that plan with Confirm { prompt: None } removes all backups.
    #[test]
    fn test_clean_removes_with_yes() {
        let dir = test_dir();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(backup_dir.join("2024-01-10T10-00-00-000")).unwrap();
        std::fs::create_dir_all(backup_dir.join("2024-01-11T10-00-00-000")).unwrap();
        std::fs::create_dir_all(backup_dir.join("2024-01-12T10-00-00-000")).unwrap();

        init_git_repo(&dir.path().to_path_buf());

        let to_remove = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-11T10-00-00-000".to_string(),
        ];

        let remove_actions: Vec<Action> = to_remove
            .iter()
            .map(|b| Action::RemoveDir {
                path: backup_dir.join(b),
            })
            .collect();

        let confirm_action = Action::Confirm {
            prompt: None,
            actions: remove_actions,
        };

        let mut plan = Plan::new(dir.path());
        plan.add(confirm_action);

        let repo_path = dir.path().to_path_buf();
        let state_path = repo_path.join("state");
        let config_path = repo_path.join("config");
        let backups_path = repo_path.join("backups");
        std::fs::create_dir_all(&state_path).unwrap();
        let mut repo = RepoState::new_for_git(repo_path, state_path, config_path, backups_path);

        plan::execute_plan(&plan, ExecuteMode::Normal, &mut repo).unwrap();

        // Targeted backups should be removed
        assert!(!backup_dir.join("2024-01-10T10-00-00-000").exists());
        assert!(!backup_dir.join("2024-01-11T10-00-00-000").exists());
        // Untargeted backup should remain
        assert!(backup_dir.join("2024-01-12T10-00-00-000").exists());
    }

    /// Test that Confirm with prompt in non-interactive context skips removal.
    #[test]
    fn test_clean_skips_when_not_confirmed() {
        let dir = test_dir();
        let backup_dir = dir.path().join("backups");
        let target = backup_dir.join("2024-01-10T10-00-00-000");
        std::fs::create_dir_all(&target).unwrap();

        init_git_repo(&dir.path().to_path_buf());

        let action = Action::Confirm {
            prompt: Some("Remove 1 backup(s)?".into()),
            actions: vec![Action::RemoveDir {
                path: target.clone(),
            }],
        };

        let repo_path = dir.path().to_path_buf();
        let state_path = repo_path.join("state");
        let config_path = repo_path.join("config");
        let backups_path = repo_path.join("backups");
        std::fs::create_dir_all(&state_path).unwrap();
        let mut repo = RepoState::new_for_git(repo_path, state_path, config_path, backups_path);

        // In CI (non-interactive), Confirm with prompt skips execution
        temp_env::with_var("CI", Some("1"), || {
            action_execute(&action, &mut repo).unwrap();
        });

        // Backup should not be removed (prompt was skipped in CI)
        assert!(target.exists());
    }

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

    #[test]
    fn test_filter_backups_keep_before_combined() {
        let backups = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-15T10-00-00-000".to_string(),
            "2024-01-20T10-00-00-000".to_string(),
            "2024-02-01T10-00-00-000".to_string(),
            "2024-02-10T10-00-00-000".to_string(),
        ];
        // --keep 2 --before 2024-02-01: 3 backups before Feb, keep the 2 newest
        let (to_remove, skip) = filter_backups(&backups, Some(2), Some("2024-02-01")).unwrap();
        assert!(skip.is_none());
        assert_eq!(to_remove.len(), 1);
        assert_eq!(to_remove[0], "2024-01-10T10-00-00-000");
    }

    #[test]
    fn test_filter_backups_keep_before_keeps_all_when_under_count() {
        let backups = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-15T10-00-00-000".to_string(),
            "2024-02-10T10-00-00-000".to_string(),
        ];
        // --keep 5 --before 2024-02-01: only 2 backups before Feb, keep both
        let (to_remove, skip) = filter_backups(&backups, Some(5), Some("2024-02-01")).unwrap();
        assert!(to_remove.is_empty());
        assert!(skip.is_some());
        assert!(
            skip.unwrap()
                .contains("Keeping all 2 backups before 2024-02-01")
        );
    }

    #[test]
    fn test_filter_backups_keep_before_invalid_date() {
        let backups = vec!["2024-01-10T10-00-00-000".to_string()];
        let result = filter_backups(&backups, Some(1), Some("bad-date"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid date format"));
    }

    #[test]
    fn test_filter_backups_keep_before_empty_set() {
        // All backups are after the cutoff, so --keep applies to empty set
        let backups = vec!["2024-03-01T10-00-00-000".to_string()];
        let (to_remove, skip) = filter_backups(&backups, Some(1), Some("2024-02-01")).unwrap();
        assert!(to_remove.is_empty());
        assert!(skip.is_some());
        assert!(
            skip.unwrap()
                .contains("Keeping all 0 backups before 2024-02-01")
        );
    }

    #[test]
    fn test_filter_backups_keep_zero_combined() {
        let backups = vec![
            "2024-01-10T10-00-00-000".to_string(),
            "2024-01-15T10-00-00-000".to_string(),
            "2024-02-10T10-00-00-000".to_string(),
        ];
        // --keep 0 --before 2024-02-01: remove all backups before Feb
        let (to_remove, skip) = filter_backups(&backups, Some(0), Some("2024-02-01")).unwrap();
        assert!(skip.is_none());
        assert_eq!(to_remove.len(), 2);
        assert_eq!(to_remove[0], "2024-01-10T10-00-00-000");
        assert_eq!(to_remove[1], "2024-01-15T10-00-00-000");
    }
}
