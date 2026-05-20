use indexmap::IndexMap;
use std::path::PathBuf;

/// Classify tracked files into tiers and merge by priority.
///
/// Returns a map from target path → (tier name, repo-relative path).
/// Higher tiers override lower tiers for the same target path.
/// Uses `IndexMap` to preserve insertion order (base → platform → machine)
/// for deterministic iteration during plan building.
pub(crate) fn merge_tiers(
    tracked_files: &[String],
    machine: &str,
    platform: &Option<String>,
) -> IndexMap<PathBuf, (String, String)> {
    let mut merged: IndexMap<PathBuf, (String, String)> = IndexMap::new();

    // Process tiers in order: base (lowest) → platform → machine (highest)
    // Later tiers overwrite earlier tiers for the same target path.

    // Tier 1: base
    for file in tracked_files {
        if let Some(_rest) = file.strip_prefix("base/") {
            let repo_path = PathBuf::from(file);
            if let Ok(target) = crate::paths::repo_to_target(&repo_path) {
                merged.insert(target, ("base".to_string(), file.clone()));
            }
        }
    }

    // Tier 2: platform
    if let Some(plat) = platform {
        let platform_prefix = format!("{}/", plat);
        for file in tracked_files {
            if let Some(_rest) = file.strip_prefix(&platform_prefix) {
                let repo_path = PathBuf::from(file);
                if let Ok(target) = crate::paths::repo_to_target(&repo_path) {
                    merged.insert(target, (plat.clone(), file.clone()));
                }
            }
        }
    }

    // Tier 3: machine (highest priority)
    let machine_prefix = format!("{}/", machine);
    for file in tracked_files {
        if let Some(_rest) = file.strip_prefix(&machine_prefix) {
            let repo_path = PathBuf::from(file);
            if let Ok(target) = crate::paths::repo_to_target(&repo_path) {
                merged.insert(target, (machine.to_string(), file.clone()));
            }
        }
    }

    merged
}

/// Build a map of which target paths are overridden by higher tiers.
///
/// Returns a map from target path → the lower tier that was overridden.
pub(crate) fn build_override_map(
    tracked_files: &[String],
    machine: &Option<String>,
    platform: &Option<String>,
) -> IndexMap<PathBuf, String> {
    let mut all_tiers: IndexMap<PathBuf, Vec<(String, String)>> = IndexMap::new();

    // Collect all tiers for each target
    for file in tracked_files {
        let repo_path = PathBuf::from(file);
        if let Ok(target) = crate::paths::repo_to_target(&repo_path) {
            let tier = crate::convention::classify_tier(file, machine, platform);
            if let Some(tier_name) = tier {
                all_tiers
                    .entry(target)
                    .or_default()
                    .push((tier_name, file.clone()));
            }
        }
    }

    // Find overrides: if a target has entries from multiple tiers, the lower ones are overridden
    let mut overrides: IndexMap<PathBuf, String> = IndexMap::new();

    for (target, entries) in &all_tiers {
        if entries.len() <= 1 {
            continue;
        }

        // Determine the highest tier present
        let highest = entries
            .iter()
            .map(|(tier, _)| crate::convention::tier_priority(tier))
            .max()
            .unwrap();

        // All entries with lower priority are overridden
        for (tier, _) in entries {
            if crate::convention::tier_priority(tier) < highest {
                overrides.insert(target.clone(), tier.clone());
            }
        }
    }

    overrides
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_test_home<F: FnOnce(&PathBuf)>(test: F)
    where
        F: FnOnce(&PathBuf),
    {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            test(&home);
        });
    }

    #[test]
    fn test_merge_tiers_basic() {
        with_test_home(|home| {
            let files = vec![
                "base/home/.vimrc".into(),
                "base/home/.gitconfig".into(),
                "macos/home/.config/skhd/skhdrc".into(),
                "macbook/home/.config/nvim/plugins.lua".into(),
            ];
            let merged = merge_tiers(&files, "macbook", &Some("macos".into()));

            assert_eq!(merged.len(), 4);

            assert!(merged.contains_key(&home.join(".vimrc")));
            assert!(merged.contains_key(&home.join(".gitconfig")));
        });
    }

    #[test]
    fn test_merge_tiers_override() {
        with_test_home(|home| {
            let files = vec![
                "base/home/.config/nvim/plugins.lua".into(),
                "macbook/home/.config/nvim/plugins.lua".into(),
            ];
            let merged = merge_tiers(&files, "macbook", &Some("macos".into()));

            let target = home.join(".config/nvim/plugins.lua");

            assert_eq!(merged.len(), 1);
            assert_eq!(merged.get(&target).unwrap().0, "macbook");
        });
    }

    #[test]
    fn test_override_map_detection() {
        with_test_home(|home| {
            let files = vec![
                "base/home/.config/nvim/plugins.lua".into(),
                "macbook/home/.config/nvim/plugins.lua".into(),
                "base/home/.vimrc".into(),
            ];
            let overrides =
                build_override_map(&files, &Some("macbook".into()), &Some("macos".into()));

            assert!(overrides.contains_key(&home.join(".config/nvim/plugins.lua")));
            assert!(!overrides.contains_key(&home.join(".vimrc")));
        });
    }
}
