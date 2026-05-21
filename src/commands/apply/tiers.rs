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
#[allow(unused_doc_comments)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Property test generators ──

    /// Generator for valid tier names: non-empty, no slashes, no dots.
    fn tier_name() -> impl Strategy<Value = String> {
        "[a-zA-Z][a-zA-Z0-9_-]{0,20}".prop_map(|s| s.to_string())
    }

    // ── Unit tests ──

    #[test]
    fn test_merge_tiers_basic() {
        crate::tests::with_test_home(|home| {
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
        crate::tests::with_test_home(|home| {
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
        crate::tests::with_test_home(|home| {
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

    // ── Property tests for merge_tiers ──

    /// Invariant: higher-tier files always override lower-tier files for
    /// the same target path. Machine > platform > base.
    proptest! {
        #[test]
        fn proptest_merge_tiers_priority_ordering(
            base_file in "base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_home| {
                let files = vec![base_file.clone(), format!("{}/{}", machine_name, &base_file[5..])];
                let merged = merge_tiers(&files, &machine_name, &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(merged.contains_key(&target), "target path should exist in merged map");

                let (tier, repo_path) = merged.get(&target).unwrap();
                assert_eq!(tier, &machine_name, "higher tier should override lower tier");
                assert_eq!(repo_path, &files[1], "repo path should be from the higher tier");
            });
        }
    }

    /// Invariant: merging the same tiers twice produces the same result (idempotency).
    proptest! {
        #[test]
        fn proptest_merge_tiers_idempotent(
            base_files in proptest::collection::vec("base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()), 1..10),
            machine_name in tier_name(),
            machine_files in proptest::collection::vec(
                "[a-zA-Z][a-zA-Z0-9_-]{0,15}/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
                1..5
            ),
        ) {
            crate::tests::with_test_home(|_| {
                let filtered: Vec<String> = machine_files
                    .into_iter()
                    .filter(|f| f.starts_with(&format!("{}/", machine_name)))
                    .collect();

                let mut all_files = base_files.clone();
                all_files.extend(filtered);

                let result1 = merge_tiers(&all_files, &machine_name, &None);
                let result2 = merge_tiers(&all_files, &machine_name, &None);

                assert_eq!(
                    &result1, &result2,
                    "merge_tiers should be idempotent: same input produces same output"
                );
            });
        }
    }

    /// Invariant: merging with empty tier lists produces an empty result.
    proptest! {
        #[test]
        fn proptest_merge_tiers_empty_input(
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let empty: Vec<String> = vec![];
                let merged = merge_tiers(&empty, &machine_name, &None);
                assert!(merged.is_empty(), "empty input should produce empty output");

                let merged_with_platform = merge_tiers(&empty, &machine_name, &Some("macos".into()));
                assert!(merged_with_platform.is_empty(), "empty input with platform should produce empty output");
            });
        }
    }

    /// Invariant: merging across all three tiers respects priority ordering.
    /// base < platform < machine for the same target.
    proptest! {
        #[test]
        fn proptest_merge_tiers_three_tier_priority(
            target_file in "home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            platform in tier_name(),
            machine in tier_name(),
        ) {
            crate::tests::with_test_home(|_home| {
                let base_path = format!("base/{}", target_file);
                let plat_path = format!("{}/{}", platform, target_file);
                let mach_path = format!("{}/{}", machine, target_file);

                let files = vec![base_path.clone(), plat_path.clone(), mach_path.clone()];
                let merged = merge_tiers(&files, &machine, &Some(platform.clone()));

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_path)).unwrap();
                assert!(merged.contains_key(&target));

                let (tier, repo_path) = merged.get(&target).unwrap();
                assert_eq!(tier, &machine, "machine tier should have highest priority");
                assert_eq!(repo_path, &mach_path);
            });
        }
    }

    /// Invariant: files from a scope that doesn't match machine/platform
    /// are ignored (not in merged result).
    proptest! {
        #[test]
        fn proptest_merge_tiers_ignores_unmatched_scope(
            base_file in "base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            other_scope in prop_oneof![
                Just("custom".to_string()),
                Just("extra".to_string()),
                Just("other".to_string()),
            ],
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let files = vec![base_file.clone(), format!("{}/{}", other_scope, &base_file[5..])];
                let merged = merge_tiers(&files, &machine_name, &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(merged.contains_key(&target));
                let (tier, _) = merged.get(&target).unwrap();
                assert_eq!(tier, "base");
            });
        }
    }

    /// Invariant: deep nesting (many path components) is handled correctly.
    proptest! {
        #[test]
        fn proptest_merge_tiers_deep_nesting(
            depth in 1usize..6,
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let components: Vec<String> = (0..depth)
                    .map(|i| format!("file_{}", i))
                    .collect();
                let file_path = components.join("/");
                let base_file = format!("base/home/{}", file_path);
                let machine_file = format!("{}/home/{}", machine_name, file_path);

                let files = vec![base_file.clone(), machine_file];
                let merged = merge_tiers(&files, &machine_name, &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(merged.contains_key(&target));
                let (tier, _) = merged.get(&target).unwrap();
                assert_eq!(tier, &machine_name);
            });
        }
    }

    /// Invariant: special characters in file names are preserved correctly.
    proptest! {
        #[test]
        fn proptest_merge_tiers_special_characters(
            name in "[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let base_file = format!("base/home/.{}", name);
                let machine_file = format!("{}/home/.{}", machine_name, name);

                let files = vec![base_file.clone(), machine_file.clone()];
                let merged = merge_tiers(&files, &machine_name, &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(merged.contains_key(&target));
                let (tier, repo_path) = merged.get(&target).unwrap();
                assert_eq!(tier, &machine_name);
                assert_eq!(repo_path, &machine_file);
            });
        }
    }

    /// Invariant: duplicate files within the same tier are handled (last one wins).
    proptest! {
        #[test]
        fn proptest_merge_tiers_duplicates_in_same_tier(
            base_file in "base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let machine_file = format!("{}/{}", machine_name, &base_file[5..]);
                let files = vec![base_file.clone(), machine_file.clone(), machine_file.clone()];
                let merged = merge_tiers(&files, &machine_name, &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(merged.contains_key(&target));
                let (tier, repo_path) = merged.get(&target).unwrap();
                assert_eq!(tier, &machine_name);
                assert_eq!(repo_path, &machine_file);
            });
        }
    }

    /// Invariant: non-overlapping files from different tiers all appear in result.
    proptest! {
        #[test]
        fn proptest_merge_tiers_non_overlapping_files(
            base_files in proptest::collection::vec("base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()), 1..8),
            machine_files in proptest::collection::vec(
                "[a-zA-Z][a-zA-Z0-9_-]{0,15}/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
                1..8
            ),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let filtered_machine: Vec<String> = machine_files
                    .into_iter()
                    .filter(|f| f.starts_with(&format!("{}/", machine_name)))
                    .collect();

                let all_files: Vec<String> = base_files.iter()
                    .cloned()
                    .chain(filtered_machine)
                    .collect();

                let merged = merge_tiers(&all_files, &machine_name, &None);

                let base_targets: std::collections::HashSet<_> = base_files.iter()
                    .filter_map(|f| crate::paths::repo_to_target(&std::path::PathBuf::from(f)).ok())
                    .collect();

                for target in base_targets {
                    assert!(merged.contains_key(&target),
                        "base file target {:?} should be in merged result", target);
                    if let Some((tier, _)) = merged.get(&target) {
                        assert_eq!(tier, "base");
                    }
                }
            });
        }
    }

    // ── Property tests for build_override_map ──

    /// Invariant: build_override_map correctly identifies all overrides.
    /// If a target has files from multiple tiers, the lower ones are in the override map.
    proptest! {
        #[test]
        fn proptest_build_override_map_detects_overrides(
            base_file in "base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let machine_file = format!("{}/{}", machine_name, &base_file[5..]);
                let files = vec![base_file.clone(), machine_file];

                let overrides = build_override_map(&files, &Some(machine_name.clone()), &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(overrides.contains_key(&target),
                    "override map should contain target with files from multiple tiers");
                assert_eq!(overrides.get(&target).unwrap(), "base",
                    "the overridden tier should be base");
            });
        }
    }

    /// Invariant: no false positives — files only in one tier are not in override map.
    proptest! {
        #[test]
        fn proptest_build_override_map_no_false_positives(
            base_file in "base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let files = vec![base_file];
                let overrides = build_override_map(&files, &Some(machine_name), &None);
                assert!(overrides.is_empty(),
                    "override map should be empty when only one file (no overrides possible)");
            });
        }
    }

    /// Invariant: empty input produces empty override map.
    proptest! {
        #[test]
        fn proptest_build_override_map_empty_input(
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let empty: Vec<String> = vec![];
                let overrides = build_override_map(&empty, &Some(machine_name), &None);
                assert!(overrides.is_empty(),
                    "empty input should produce empty override map");
            });
        }
    }

    /// Invariant: three-tier override map correctly identifies base and platform as overridden.
    proptest! {
        #[test]
        fn proptest_build_override_map_three_tier_overrides(
            target_file in "home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            platform in tier_name(),
            machine in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let base_path = format!("base/{}", target_file);
                let plat_path = format!("{}/{}", platform, target_file);
                let mach_path = format!("{}/{}", machine, target_file);

                let files = vec![base_path.clone(), plat_path.clone(), mach_path.clone()];
                let overrides = build_override_map(&files, &Some(machine.clone()), &Some(platform.clone()));

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_path)).unwrap();
                assert!(overrides.contains_key(&target),
                    "override map should contain the target");
                let overridden = overrides.get(&target).unwrap();
                assert!(overridden == "base" || overridden == &platform,
                    "overridden tier should be base or platform, got: {}", overridden);
            });
        }
    }

    /// Invariant: override map is deterministic — same input always gives same result.
    proptest! {
        #[test]
        fn proptest_build_override_map_deterministic(
            files in proptest::collection::vec("[a-zA-Z]/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()), 3..15),
            machine in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let filtered: Vec<String> = files.iter()
                    .cloned()
                    .filter(|f| f.starts_with(&format!("{}/", machine)))
                    .collect();

                let result1 = build_override_map(&filtered, &Some(machine.clone()), &None);
                let result2 = build_override_map(&filtered, &Some(machine.clone()), &None);

                assert_eq!(
                    &result1, &result2,
                    "build_override_map should be deterministic"
                );
            });
        }
    }

    /// Invariant: deep nesting paths are handled correctly in override detection.
    proptest! {
        #[test]
        fn proptest_build_override_map_deep_nesting(
            depth in 1usize..6,
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let components: Vec<String> = (0..depth)
                    .map(|i| format!("file_{}", i))
                    .collect();
                let file_path = components.join("/");
                let base_file = format!("base/home/{}", file_path);
                let machine_file = format!("{}/home/{}", machine_name, file_path);

                let files = vec![base_file.clone(), machine_file];
                let overrides = build_override_map(&files, &Some(machine_name), &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(overrides.contains_key(&target),
                    "override map should detect override for deeply nested path");
            });
        }
    }

    /// Invariant: special characters in file names are preserved in override map keys.
    proptest! {
        #[test]
        fn proptest_build_override_map_special_characters(
            name in "[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let base_file = format!("base/home/.{}", name);
                let machine_file = format!("{}/home/.{}", machine_name, name);

                let files = vec![base_file.clone(), machine_file];
                let overrides = build_override_map(&files, &Some(machine_name), &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(overrides.contains_key(&target),
                    "override map should contain target with special characters");
            });
        }
    }

    /// Invariant: build_override_map handles files with dotdot components gracefully.
    /// Note: repo_to_target does NOT canonicalize, so ".." is preserved in the path.
    proptest! {
        #[test]
        fn proptest_build_override_map_with_dotdot_in_path(
            name in "[a-zA-Z0-9_.@-]{2,20}".prop_map(|s| s.to_string()),
            machine_name in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let base_file = format!("base/home/dir/../{}", name);
                let machine_file = format!("{}/home/dir/../{}", machine_name, name);

                let files = vec![base_file.clone(), machine_file];
                let overrides = build_override_map(&files, &Some(machine_name), &None);

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                assert!(overrides.contains_key(&target),
                    "override map should handle paths with '..' components");
            });
        }
    }

    /// Invariant: merging with only platform tier (no machine) still detects overrides.
    proptest! {
        #[test]
        fn proptest_build_override_map_platform_overrides_base(
            target_file in "home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            platform in tier_name(),
        ) {
            crate::tests::with_test_home(|_| {
                let base_path = format!("base/{}", target_file);
                let plat_path = format!("{}/{}", platform, target_file);

                let files = vec![base_path.clone(), plat_path.clone()];
                let overrides = build_override_map(&files, &None, &Some(platform.clone()));

                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_path)).unwrap();
                assert!(overrides.contains_key(&target),
                    "platform should override base");
                assert_eq!(overrides.get(&target).unwrap(), "base",
                    "overridden tier should be base");
            });
        }
    }
}
