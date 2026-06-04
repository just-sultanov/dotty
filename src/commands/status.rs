use indexmap::IndexMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;
use crate::convention::classify_tier;
use crate::error::DottyError;
use crate::fs_utils::calculate_dir_size;
use crate::git;
use crate::paths::{expand_tilde, format_target_display, repo_to_target};
use crate::platform::detect_platform;
use crate::repo_state::RepoState;
use crate::symlink::is_symlink;

/// Run the `status` command.
pub fn run() -> Result<()> {
    let repo = RepoState::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    repo.require_git().map_err(|e| anyhow::anyhow!("{e}"))?;

    let repo_path = &repo.repo_path;
    let state_path = &repo.state_path;

    // Read config
    let config = &repo.config;

    // Detect platform
    let platform = detect_platform();

    // Display basic info
    println!(
        "Machine:   {}",
        config.machine.as_deref().unwrap_or("(not set)")
    );
    println!("Platform:  {}", platform.as_deref().unwrap_or("(unknown)"));
    println!("Repo:      {}", repo_path.display());

    // Current branch
    if let Ok(branch) = git::git_current_branch(repo_path) {
        println!("Branch:    {}", branch);
    }

    // Git dirty status
    // Error handling strategy: print warning to stderr and continue with partial output.
    // The status command is a diagnostic tool — we report what we can rather than aborting.
    let git_status = git_status_summary(repo_path);
    match git_status {
        Ok(summary) => println!("Git:       {}", summary),
        Err(e) => {
            eprintln!("{} Failed to read git status: {e}", crate::symbols::warn());
            println!("Git:       (unavailable)");
        }
    }

    // Broken symlinks
    let broken = find_broken_symlinks(repo_path, config);
    if broken.is_empty() {
        println!("Broken:    0");
    } else {
        println!("Broken:    {}", broken.len());
        for (target, repo_relative_path, reason) in &broken {
            println!("  {} → {} ({})", target, repo_relative_path, reason);
        }
    }

    // Backup size
    let backup_size = calculate_dir_size(&state_path.join("backups"));
    let backup_entries = count_backup_entries(state_path);
    if backup_size > 0 {
        let size_mb = backup_size as f64 / (1024.0 * 1024.0);
        println!("Backups:   {:.1} MB ({} entries)", size_mb, backup_entries);
        if backup_size > 50 * 1024 * 1024 {
            println!(
                "  {} Consider running `dotty clean`",
                crate::symbols::warn()
            );
        }
    } else {
        println!("Backups:   0 MB");
    }

    // Tier conflicts
    let conflicts = find_tier_conflicts(repo_path, &config.machine, &platform);
    if conflicts.is_empty() {
        println!("Conflicts: 0");
    } else {
        println!("Conflicts: {}", conflicts.len());
        for (target, overriding, overridden) in &conflicts {
            println!("  {}: {} overrides {}", target, overriding, overridden);
        }
    }

    // Inactive tier overrides
    let inactive = find_inactive_tiers(repo_path, &config.machine, &platform);
    if inactive.is_empty() {
        println!("Inactive:  0");
    } else {
        println!("Inactive:  {}", inactive.len());
        for (target, tier, repo_relative_path) in &inactive {
            println!(
                "  {} (tier: {}, file: {})",
                target, tier, repo_relative_path
            );
        }
    }

    Ok(())
}

/// Summarize git status as a human-readable string.
///
/// Returns `DottyError::Git` if the git command fails (e.g., git not installed,
/// corrupted repo, insufficient permissions). The caller is responsible for
/// deciding whether to abort or continue with partial output.
fn git_status_summary(repo_path: &Path) -> Result<String, DottyError> {
    let porcelain = git::git_status(repo_path)?;

    if porcelain.is_empty() {
        return Ok("clean".to_string());
    }

    let mut modified = 0usize;
    let mut added = 0usize;
    let mut deleted = 0usize;
    let mut untracked = 0usize;

    for line in porcelain.lines() {
        if line.len() >= 2 {
            let status = &line[..2];
            match status {
                "M " | "MM" => modified += 1,
                "A " | "AA" => added += 1,
                "D " | "DD" => deleted += 1,
                "??" => untracked += 1,
                _ => {
                    // Modified in index, staged, etc.
                    if status.contains('M') {
                        modified += 1;
                    } else if status.contains('A') {
                        added += 1;
                    } else if status.contains('D') {
                        deleted += 1;
                    }
                }
            }
        }
    }

    let mut parts = Vec::new();
    if modified > 0 {
        parts.push(format!("{} modified", modified));
    }
    if added > 0 {
        parts.push(format!("{} added", added));
    }
    if deleted > 0 {
        parts.push(format!("{} deleted", deleted));
    }
    if untracked > 0 {
        parts.push(format!("{} untracked", untracked));
    }

    if parts.is_empty() {
        Ok("clean".to_string())
    } else {
        Ok(parts.join(", "))
    }
}

/// Find broken symlinks from the managed map.
///
/// Returns a list of (target_path, repo_relative_path, reason).
fn find_broken_symlinks(_repo_path: &Path, config: &Config) -> Vec<(String, String, String)> {
    let mut broken = Vec::new();

    for (repo_relative_path, target_ref) in &config.managed {
        let target = match expand_tilde(target_ref) {
            Ok(t) => t,
            Err(_) => {
                broken.push((
                    target_ref.clone(),
                    repo_relative_path.clone(),
                    "invalid target path".to_string(),
                ));
                continue;
            }
        };

        // Check if symlink exists
        if !is_symlink(&target) {
            continue;
        }

        // Check if the symlink target (repo file) exists and is reachable
        // fs::metadata follows symlinks, so calling it on the symlink path
        // detects dangling symlinks (NotFound), permission errors, etc.
        match fs::metadata(&target) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                broken.push((
                    target_ref.clone(),
                    repo_relative_path.clone(),
                    "target not found (dangling symlink)".to_string(),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                broken.push((
                    target_ref.clone(),
                    repo_relative_path.clone(),
                    format!("target unreachable: {}", e),
                ));
            }
            Err(e) => {
                broken.push((
                    target_ref.clone(),
                    repo_relative_path.clone(),
                    format!("target error: {}", e),
                ));
            }
        }
    }

    broken
}

/// Count total backup entries across all backup directories.
fn count_backup_entries(state_path: &Path) -> usize {
    let backup_dir = state_path.join("backups");

    if !backup_dir.is_dir() {
        return 0;
    }

    let mut count = 0usize;

    for entry in std::fs::read_dir(&backup_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_dir()
            && let Ok(entries) = std::fs::read_dir(&path)
        {
            count += entries.count();
        }
    }

    count
}

/// Find tier conflicts — paths present in multiple tiers.
fn find_tier_conflicts(
    repo_path: &Path,
    machine: &Option<String>,
    platform: &Option<String>,
) -> Vec<(String, String, String)> {
    let tracked_files = match git::TrackedFiles::new(repo_path) {
        Ok(iter) => iter.collect::<Vec<_>>(),
        Err(_) => return Vec::new(),
    };

    // Collect all tiers for each target path.
    // IndexMap preserves insertion order, ensuring deterministic conflict output.
    let mut all_tiers: IndexMap<PathBuf, Vec<(String, String)>> = IndexMap::new();

    for file in &tracked_files {
        let repo_path_buf = PathBuf::from(file);
        if let Ok(target) = repo_to_target(&repo_path_buf) {
            let tier = classify_tier(file, machine, platform);
            if let Some(tier_name) = tier {
                all_tiers
                    .entry(target)
                    .or_default()
                    .push((tier_name, file.clone()));
            }
        }
    }

    // Find paths with multiple tiers
    let mut conflicts = Vec::new();

    for (target, entries) in &all_tiers {
        if entries.len() <= 1 {
            continue;
        }

        // Find the highest priority tier
        let highest = entries
            .iter()
            .map(|(tier, _)| tier_priority(tier))
            .max()
            .unwrap();

        // Report each override
        for (tier, _repo_relative_path) in entries {
            if tier_priority(tier) < highest {
                // Find what overrides this
                let overriding = entries
                    .iter()
                    .find(|(t, _)| tier_priority(t) == highest)
                    .map(|(t, _)| t.clone())
                    .unwrap_or_default();

                let target_str = format_target_display(target);
                conflicts.push((target_str, overriding, tier.clone()));
            }
        }
    }

    conflicts
}

/// Return a numeric priority for a tier name (higher = more priority).
fn tier_priority(tier: &str) -> u32 {
    crate::convention::tier_priority(tier)
}

/// Find files in inactive tiers (platforms/machines not active on this system).
///
/// Returns a list of (target_path, tier_name, repo_relative_path).
fn find_inactive_tiers(
    repo_path: &Path,
    machine: &Option<String>,
    platform: &Option<String>,
) -> Vec<(String, String, String)> {
    let tracked_files = match git::TrackedFiles::new(repo_path) {
        Ok(iter) => iter.collect::<Vec<_>>(),
        Err(_) => return Vec::new(),
    };

    let mut inactive = Vec::new();

    for file in &tracked_files {
        // Extract tier from the first path component
        let tier = match file.split('/').next() {
            Some("base") => continue, // base is always active
            Some(t) => t.to_string(),
            None => continue,
        };

        // Check if this tier is active
        let is_active =
            platform.as_deref() == Some(tier.as_str()) || machine.as_deref() == Some(tier.as_str());

        if !is_active {
            let repo_path_buf = PathBuf::from(file);
            if let Ok(target) = repo_to_target(&repo_path_buf) {
                let target_str = format_target_display(&target);
                inactive.push((target_str, tier, file.clone()));
            }
        }
    }

    inactive
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for tier_priority and classify_tier live in convention.rs.

    #[test]
    fn test_format_target_display_home() {
        crate::tests::with_test_home(|home| {
            let path = home.join(".vimrc");
            let formatted = format_target_display(&path);
            assert_eq!(formatted, "~/.vimrc");
        });
    }

    #[test]
    fn test_format_target_display_absolute() {
        let path = PathBuf::from("/opt/nvim/appimage");
        let formatted = format_target_display(&path);
        assert_eq!(formatted, "/opt/nvim/appimage");
    }

    #[test]
    fn test_format_target_display_tilde_only() {
        let path = PathBuf::from("~");
        let formatted = format_target_display(&path);
        // ~ without home_dir match stays as-is
        assert_eq!(formatted, "~");
    }

    #[test]
    fn test_count_backup_entries_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path();
        assert_eq!(count_backup_entries(state_path), 0);
    }

    #[test]
    fn test_count_backup_entries_with_backups() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path();
        let backup_dir = state_path.join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // Create two backup snapshots, each with files
        let snap1 = backup_dir.join("2024-01-15T10-00-00-000");
        let snap2 = backup_dir.join("2024-01-16T10-00-00-000");
        std::fs::create_dir_all(&snap1).unwrap();
        std::fs::create_dir_all(&snap2).unwrap();
        std::fs::write(snap1.join("vimrc.bak"), "content").unwrap();
        std::fs::write(snap2.join("nvim.bak"), "content").unwrap();
        std::fs::write(snap2.join("gitconfig.bak"), "content").unwrap();

        assert_eq!(count_backup_entries(state_path), 3);
    }

    #[test]
    fn test_count_backup_entries_ignores_files_in_root() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path();
        let backup_dir = state_path.join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // A file directly in backups/ should be ignored (only subdirs count)
        std::fs::write(backup_dir.join("readme.txt"), "info").unwrap();
        let snap = backup_dir.join("2024-01-15T10-00-00-000");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(snap.join("vimrc.bak"), "content").unwrap();

        assert_eq!(count_backup_entries(state_path), 1);
    }

    #[test]
    fn test_git_status_summary_returns_error_on_non_repo() {
        // A directory without a git repo should cause git_status to fail,
        // which should propagate as a DottyError::Git.
        let dir = tempfile::tempdir().unwrap();
        let result = git_status_summary(dir.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            DottyError::Git { stderr, .. } => {
                assert!(
                    stderr.contains("git status") && stderr.contains("failed"),
                    "expected git status error, got: {stderr}"
                );
            }
            other => panic!("expected DottyError::Git, got {other:?}"),
        }
    }
}
