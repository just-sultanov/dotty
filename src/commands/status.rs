use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::convention::{
    self, calculate_dir_size, expand_target_ref, read_config, repo_to_target, resolve_repo_path,
    resolve_state_path,
};
use crate::git;
use crate::symlink::is_symlink;

/// Run the `status` command.
pub fn run() -> Result<()> {
    let repo_path = resolve_repo_path()?;
    let state_path = resolve_state_path()?;

    // Ensure repo exists
    if !repo_path.join(".git").exists() {
        anyhow::bail!(
            "No dotty repository found at {}. Run `dotty init` first.",
            repo_path.display()
        );
    }

    // Read config
    let config = read_config(&state_path)?;

    // Detect platform
    let platform = convention::detect_platform();

    // Display basic info
    println!(
        "Machine:   {}",
        config.machine.as_deref().unwrap_or("(not set)")
    );
    println!("Platform:  {}", platform.as_deref().unwrap_or("(unknown)"));
    println!("Repo:      {}", repo_path.display());

    // Current branch
    if let Ok(branch) = git::git_current_branch(&repo_path) {
        println!("Branch:    {}", branch);
    }

    // Git dirty status
    let git_status = git_status_summary(&repo_path);
    println!("Git:       {}", git_status);

    // Broken symlinks
    let broken = find_broken_symlinks(&config);
    if broken.is_empty() {
        println!("Broken:    0");
    } else {
        println!("Broken:    {}", broken.len());
        for (target, repo_rel, reason) in &broken {
            println!("  {} → {} ({})", target, repo_rel, reason);
        }
    }

    // Backup size
    let backup_size = calculate_dir_size(&state_path.join("backups"));
    let backup_entries = count_backup_entries(&state_path);
    if backup_size > 0 {
        let size_mb = backup_size as f64 / (1024.0 * 1024.0);
        println!("Backups:   {:.1} MB ({} entries)", size_mb, backup_entries);
        if backup_size > 50 * 1024 * 1024 {
            println!("  Consider running `dotty clean`");
        }
    } else {
        println!("Backups:   0 MB");
    }

    // Tier conflicts
    let conflicts = find_tier_conflicts(&repo_path, &config.machine, &platform);
    if conflicts.is_empty() {
        println!("Conflicts: 0");
    } else {
        println!("Conflicts: {}", conflicts.len());
        for (target, overriding, overridden) in &conflicts {
            println!("  {}: {} overrides {}", target, overriding, overridden);
        }
    }

    Ok(())
}

/// Summarize git status as a human-readable string.
fn git_status_summary(repo_path: &Path) -> String {
    let porcelain = match git::git_status(repo_path) {
        Ok(p) => p,
        Err(_) => return "(error)".to_string(),
    };

    if porcelain.is_empty() {
        return "clean".to_string();
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
        "clean".to_string()
    } else {
        parts.join(", ")
    }
}

/// Find broken symlinks from the managed map.
///
/// Returns a list of (target_path, repo_rel_path, reason).
fn find_broken_symlinks(config: &crate::convention::Config) -> Vec<(String, String, String)> {
    let mut broken = Vec::new();

    for (repo_rel, target_ref) in &config.managed {
        let target = match expand_target_ref(target_ref) {
            Ok(t) => t,
            Err(_) => {
                broken.push((
                    target_ref.clone(),
                    repo_rel.clone(),
                    "invalid target path".to_string(),
                ));
                continue;
            }
        };

        // Check if symlink exists
        if !is_symlink(&target) {
            continue;
        }

        // Check if the symlink target (repo file) exists
        let repo_file = resolve_repo_path().unwrap_or_default().join(repo_rel);
        if !repo_file.exists() {
            broken.push((target_ref.clone(), repo_rel.clone(), "missing".to_string()));
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
    let tracked_files = match git::git_ls_files(repo_path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    // Collect all tiers for each target path
    let mut all_tiers: HashMap<PathBuf, Vec<(String, String)>> = HashMap::new();

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
        for (tier, _repo_rel) in entries {
            if tier_priority(tier) < highest {
                // Find what overrides this
                let overriding = entries
                    .iter()
                    .find(|(t, _)| tier_priority(t) == highest)
                    .map(|(t, _)| t.clone())
                    .unwrap_or_default();

                let target_str = format_target_path(target);
                conflicts.push((target_str, overriding, tier.clone()));
            }
        }
    }

    conflicts
}

/// Format a target path for display (with ~ prefix for home paths).
fn format_target_path(path: &Path) -> String {
    let home = match convention::home_dir() {
        Ok(h) => h,
        Err(_) => return path.display().to_string(),
    };

    if let Ok(relative) = path.strip_prefix(&home) {
        if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{relative}", relative = relative.display())
        }
    } else {
        path.display().to_string()
    }
}

/// Classify a repo-relative path into its tier.
fn classify_tier(
    file: &str,
    machine: &Option<String>,
    platform: &Option<String>,
) -> Option<String> {
    if file.starts_with("base/") {
        return Some("base".to_string());
    }

    if let Some(plat) = platform {
        let platform_prefix = format!("{}/", plat);
        if file.starts_with(&platform_prefix) {
            return Some(plat.clone());
        }
    }

    if let Some(mach) = machine {
        let machine_prefix = format!("{}/", mach);
        if file.starts_with(&machine_prefix) {
            return Some(mach.to_string());
        }
    }

    None
}

/// Return a numeric priority for a tier name (higher = more priority).
fn tier_priority(tier: &str) -> u32 {
    if tier == "base" {
        return 1;
    }
    if convention::KNOWN_PLATFORMS.contains(&tier) {
        return 2;
    }
    3 // machine tier
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_priority() {
        assert_eq!(tier_priority("base"), 1);
        assert_eq!(tier_priority("macos"), 2);
        assert_eq!(tier_priority("linux"), 2);
        assert_eq!(tier_priority("freebsd"), 2);
        assert_eq!(tier_priority("macbook"), 3);
        assert_eq!(tier_priority("ubuntu-work"), 3);
    }

    #[test]
    fn test_classify_tier_base() {
        assert_eq!(
            classify_tier(
                "base/home/.vimrc",
                &Some("macbook".into()),
                &Some("macos".into())
            ),
            Some("base".into())
        );
    }

    #[test]
    fn test_classify_tier_platform() {
        assert_eq!(
            classify_tier(
                "macos/home/.config/skhd/skhdrc",
                &Some("macbook".into()),
                &Some("macos".into())
            ),
            Some("macos".into())
        );
    }

    #[test]
    fn test_classify_tier_machine() {
        assert_eq!(
            classify_tier(
                "macbook/home/.config/nvim/plugins.lua",
                &Some("macbook".into()),
                &Some("macos".into())
            ),
            Some("macbook".into())
        );
    }

    #[test]
    fn test_format_target_path_home() {
        let home = convention::home_dir().unwrap();
        let path = home.join(".vimrc");
        let formatted = format_target_path(&path);
        assert_eq!(formatted, "~/.vimrc");
    }

    #[test]
    fn test_format_target_path_absolute() {
        let path = PathBuf::from("/opt/nvim/appimage");
        let formatted = format_target_path(&path);
        assert_eq!(formatted, "/opt/nvim/appimage");
    }
}
