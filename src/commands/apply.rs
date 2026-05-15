use indexmap::IndexMap;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::warn;

use crate::convention::{
    backup_timestamp, expand_target_ref, read_config, repo_to_target, resolve_repo_path,
    resolve_state_path, scan_machine_directories, write_config,
};
use crate::git;
use crate::plan::{self, Action, Plan};
use crate::prompt::prompt_machine_selection;
use crate::symlink::is_symlink;

/// Run the `apply` command.
pub fn run(dry_run: bool) -> Result<()> {
    let repo_path = resolve_repo_path()?;
    let state_path = resolve_state_path()?;

    // Ensure repo exists
    if !repo_path.join(".git").exists() {
        anyhow::bail!(
            "No dotty repository found at {}. Run `dotty init` first.",
            repo_path.display()
        );
    }

    // Read config (machine + managed map)
    let mut config = read_config(&state_path)?;

    // Detect platform
    let platform = crate::convention::detect_platform();

    // Resolve machine name — prompt if missing
    let machine_name = resolve_machine(&repo_path, &mut config, &state_path, dry_run)?;

    // Collect all tracked files from git
    let tracked_files = git::git_ls_files(&repo_path)?;

    // Classify files by tier and merge by priority
    let merged = merge_tiers(&tracked_files, &machine_name, &platform);

    // Build plan
    let mut plan = Plan::new("apply", &repo_path);

    // Capture current branch
    if let Ok(branch) = git::git_current_branch(&repo_path) {
        plan.branch = branch;
    }

    // Collect per-file results for console output
    let mut file_results: Vec<FileResult> = Vec::new();

    // Build override map: target_path → lower tier that was overridden
    let override_map = build_override_map(&tracked_files, &Some(machine_name.clone()), &platform);

    // Process each merged file
    for (target_path, (tier, repo_rel)) in &merged {
        let repo_file = repo_path.join(repo_rel);
        let target = target_path.to_path_buf();

        // Check target state
        let state = match inspect_target(&target, &repo_file) {
            TargetState::Correct => {
                file_results.push(FileResult {
                    target: target.clone(),
                    tier: tier.clone(),
                    applied: false,
                    skipped: true,
                    overrides: override_map.get(target_path).cloned(),
                });
                continue;
            }
            TargetState::NeedsSymlink => {
                // Create parent dirs
                if let Some(parent) = target.parent() {
                    plan.add(Action::CreateDir {
                        path: parent.to_path_buf(),
                    });
                }
                plan.add(Action::CreateSymlink {
                    target: repo_file.clone(),
                    link: target.clone(),
                });
                TargetState::NeedsSymlink
            }
            TargetState::NeedsBackup => {
                // Create parent dirs
                if let Some(parent) = target.parent() {
                    plan.add(Action::CreateDir {
                        path: parent.to_path_buf(),
                    });
                }
                // Backup the existing file
                let home = crate::convention::home_dir()?;
                let backup_base = state_path.join("backups");
                let backup_ts = backup_timestamp();
                let backup_dest = if let Ok(relative) = target.strip_prefix(&home) {
                    backup_base.join(&backup_ts).join(relative)
                } else {
                    backup_base
                        .join(&backup_ts)
                        .join(target.file_name().unwrap_or_default())
                };
                plan.add(Action::Backup {
                    source: target.clone(),
                    dest: backup_dest,
                });
                plan.add(Action::CreateSymlink {
                    target: repo_file.clone(),
                    link: target.clone(),
                });
                TargetState::NeedsBackup
            }
        };

        // Track overrides
        let overrides = if override_map.contains_key(target_path) {
            override_map.get(target_path).cloned()
        } else {
            None
        };

        file_results.push(FileResult {
            target: target.clone(),
            tier: tier.clone(),
            applied: state != TargetState::Correct,
            skipped: false,
            overrides,
        });
    }

    // Orphan detection: managed entries not in tracked files
    let tracked_set: std::collections::HashSet<&String> = tracked_files.iter().collect();
    let mut orphans: Vec<(String, String)> = Vec::new();
    for (repo_rel, target_rel) in &config.managed {
        if !tracked_set.contains(repo_rel) {
            orphans.push((repo_rel.clone(), target_rel.clone()));
        }
    }

    // Remove orphan symlinks
    for (_repo_rel, target_rel) in &orphans {
        let target = expand_target_ref(target_rel)?;
        plan.add(Action::RemoveSymlink { path: target });
    }

    // Execute plan
    plan::execute_plan(&plan, dry_run)?;

    // Print per-file summary before writing config — summary should always appear
    // even if config write fails (e.g. permission issue on state dir).
    print_per_file_summary(&file_results, &orphans, dry_run);

    // Rebuild managed map from tracked files
    // TODO: incremental update instead of full rebuild
    if !dry_run {
        let new_managed = rebuild_managed_map(&tracked_files);
        config.managed = new_managed;
        if let Err(e) = write_config(&state_path, &config) {
            warn!("failed to write config: {e}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Target state inspection
// ---------------------------------------------------------------------------

/// The state of a target path on disk.
#[derive(PartialEq)]
enum TargetState {
    /// Symlink exists and points to the correct repo file.
    Correct,
    /// Target doesn't exist or is a wrong symlink — needs a new symlink.
    NeedsSymlink,
    /// Target is a regular file — needs backup before symlink replacement.
    NeedsBackup,
}

/// Inspect the target path and determine what action is needed.
fn inspect_target(target: &Path, expected_repo_file: &Path) -> TargetState {
    if is_symlink(target) {
        if let Ok(link_target) = fs::read_link(target)
            && link_target == expected_repo_file
        {
            return TargetState::Correct;
        }
        TargetState::NeedsSymlink
    } else if target.exists() {
        TargetState::NeedsBackup
    } else {
        TargetState::NeedsSymlink
    }
}

// ---------------------------------------------------------------------------
// Tier classification and merge
// ---------------------------------------------------------------------------

/// Classify tracked files into tiers and merge by priority.
///
/// Returns a map from target path → (tier name, repo-relative path).
/// Higher tiers override lower tiers for the same target path.
fn merge_tiers(
    tracked_files: &[String],
    machine: &str,
    platform: &Option<String>,
) -> HashMap<PathBuf, (String, String)> {
    let mut merged: HashMap<PathBuf, (String, String)> = HashMap::new();

    // Process tiers in order: base (lowest) → platform → machine (highest)
    // Later tiers overwrite earlier tiers for the same target path.

    // Tier 1: base
    for file in tracked_files {
        if let Some(_rest) = file.strip_prefix("base/") {
            let repo_path = PathBuf::from(file);
            if let Ok(target) = repo_to_target(&repo_path) {
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
                if let Ok(target) = repo_to_target(&repo_path) {
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
            if let Ok(target) = repo_to_target(&repo_path) {
                merged.insert(target, (machine.to_string(), file.clone()));
            }
        }
    }

    merged
}

/// Build a map of which target paths are overridden by higher tiers.
///
/// Returns a map from target path → the lower tier that was overridden.
fn build_override_map(
    tracked_files: &[String],
    machine: &Option<String>,
    platform: &Option<String>,
) -> HashMap<PathBuf, String> {
    let mut all_tiers: HashMap<PathBuf, Vec<(String, String)>> = HashMap::new();

    // Collect all tiers for each target
    for file in tracked_files {
        let repo_path = PathBuf::from(file);
        if let Ok(target) = repo_to_target(&repo_path) {
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
    let mut overrides: HashMap<PathBuf, String> = HashMap::new();

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

// ---------------------------------------------------------------------------
// Machine resolution
// ---------------------------------------------------------------------------

/// Resolve the machine name. If missing from config, prompt user to select.
fn resolve_machine(
    repo_path: &Path,
    config: &mut crate::convention::Config,
    state_path: &Path,
    dry_run: bool,
) -> Result<String> {
    if let Some(name) = &config.machine {
        return Ok(name.clone());
    }

    // No machine in config — scan repo for known machines
    let known = scan_machine_directories(repo_path);

    if dry_run {
        if known.is_empty() {
            anyhow::bail!(
                "No machine configured and no known machines in repo. \
                 Run `dotty init` or `dotty config machine <name>` first."
            );
        }
        anyhow::bail!(
            "No machine configured. Known machines in repo: {}. \
             Run `dotty config machine <name>` to select one.",
            known.join(", ")
        );
    }

    let name = prompt_machine_selection(&known)?;
    config.machine = Some(name.clone());
    write_config(state_path, config)?;
    Ok(name)
}

// ---------------------------------------------------------------------------
// Managed map
// ---------------------------------------------------------------------------

/// Rebuild the managed map from tracked files.
fn rebuild_managed_map(tracked_files: &[String]) -> IndexMap<String, String> {
    let mut managed = IndexMap::new();
    let home = match crate::convention::home_dir() {
        Ok(h) => h,
        Err(_) => return managed,
    };

    for file in tracked_files {
        let repo_path = PathBuf::from(file);
        if let Ok(target) = repo_to_target(&repo_path) {
            let target_str = if let Ok(relative) = target.strip_prefix(&home) {
                format!("~/{relative}", relative = relative.display())
            } else {
                target.to_string_lossy().to_string()
            };
            managed.insert(file.clone(), target_str);
        }
    }

    managed
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Console output
// ---------------------------------------------------------------------------

/// Per-file result for console output.
struct FileResult {
    target: PathBuf,
    tier: String,
    applied: bool,
    skipped: bool,
    overrides: Option<String>,
}

/// Print per-file apply results in the format specified by the spec.
///
/// Format:
/// ```text
///   ✓ ~/.gitconfig (base)
///   ✓ ~/.config/nvim/plugins.lua (macbook ← overrides base)
///   ────────────────────────────────────────
///   3 applied, 1 override, 2 skipped (unchanged)
/// ```
fn print_per_file_summary(
    file_results: &[FileResult],
    orphans: &[(String, String)],
    dry_run: bool,
) {
    let prefix = if dry_run { "[dry-run] " } else { "" };
    let check = crate::symbols::check();
    let arrow = crate::symbols::arrow();

    // Print orphan removals first
    if !orphans.is_empty() {
        for (_repo_rel, target_rel) in orphans {
            println!("  {}{} orphan removed", prefix, target_rel);
        }
    }

    // Sort results by target path for consistent output
    let mut sorted = file_results.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.target.cmp(&b.target));

    let mut applied_count = 0;
    let mut override_count = 0;
    let mut skipped_count = 0;

    for result in &sorted {
        let target_str = if let Ok(home) = crate::convention::home_dir() {
            if let Ok(relative) = result.target.strip_prefix(&home) {
                format!("~/{relative}", relative = relative.display())
            } else {
                result.target.to_string_lossy().to_string()
            }
        } else {
            result.target.to_string_lossy().to_string()
        };

        if result.skipped {
            skipped_count += 1;
            continue;
        }

        if result.applied {
            applied_count += 1;
        }

        let override_info = if let Some(ref lower_tier) = result.overrides {
            override_count += 1;
            format!(" {} {} {}", arrow, lower_tier, arrow)
        } else {
            String::new()
        };

        println!("  {}{} {} ({})", prefix, check, target_str, result.tier);

        if !override_info.is_empty() {
            println!(
                "  {}  (overrides {})",
                prefix,
                result.overrides.as_ref().unwrap()
            );
        }
    }

    let separator = "────────────────────────────────────────";
    println!("  {}{}", prefix, separator);

    if dry_run {
        println!(
            "  {}{} would be applied, {} override, {} skipped (unchanged)",
            prefix, applied_count, override_count, skipped_count
        );
        println!("  {}no changes made", prefix);
    } else {
        println!(
            "  {} applied, {} override, {} skipped (unchanged)",
            applied_count, override_count, skipped_count
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_priority() {
        assert_eq!(crate::convention::tier_priority("base"), 1);
        assert_eq!(crate::convention::tier_priority("macos"), 2);
        assert_eq!(crate::convention::tier_priority("linux"), 2);
        assert_eq!(crate::convention::tier_priority("freebsd"), 2);
        assert_eq!(crate::convention::tier_priority("macbook"), 3);
        assert_eq!(crate::convention::tier_priority("ubuntu-work"), 3);
    }

    #[test]
    fn test_classify_tier_base() {
        assert_eq!(
            crate::convention::classify_tier(
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
            crate::convention::classify_tier(
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
            crate::convention::classify_tier(
                "macbook/home/.config/nvim/plugins.lua",
                &Some("macbook".into()),
                &Some("macos".into())
            ),
            Some("macbook".into())
        );
    }

    #[test]
    fn test_classify_tier_unknown() {
        assert_eq!(
            crate::convention::classify_tier(
                "random/file.txt",
                &Some("macbook".into()),
                &Some("macos".into())
            ),
            None
        );
    }

    #[test]
    fn test_merge_tiers_basic() {
        let files = vec![
            "base/home/.vimrc".into(),
            "base/home/.gitconfig".into(),
            "macos/home/.config/skhd/skhdrc".into(),
            "macbook/home/.config/nvim/plugins.lua".into(),
        ];
        let merged = merge_tiers(&files, "macbook", &Some("macos".into()));

        assert_eq!(merged.len(), 4);

        // Check that base files are classified correctly
        let home = crate::convention::home_dir().unwrap();
        assert!(merged.contains_key(&home.join(".vimrc")));
        assert!(merged.contains_key(&home.join(".gitconfig")));
    }

    #[test]
    fn test_merge_tiers_override() {
        let files = vec![
            "base/home/.config/nvim/plugins.lua".into(),
            "macbook/home/.config/nvim/plugins.lua".into(),
        ];
        let merged = merge_tiers(&files, "macbook", &Some("macos".into()));

        let home = crate::convention::home_dir().unwrap();
        let target = home.join(".config/nvim/plugins.lua");

        // Should have only one entry (machine tier wins)
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.get(&target).unwrap().0, "macbook");
    }

    #[test]
    fn test_override_map_detection() {
        let files = vec![
            "base/home/.config/nvim/plugins.lua".into(),
            "macbook/home/.config/nvim/plugins.lua".into(),
            "base/home/.vimrc".into(),
        ];
        let overrides = build_override_map(&files, &Some("macbook".into()), &Some("macos".into()));

        let home = crate::convention::home_dir().unwrap();
        assert!(overrides.contains_key(&home.join(".config/nvim/plugins.lua")));
        assert!(!overrides.contains_key(&home.join(".vimrc")));
    }

    #[test]
    fn test_inspect_target_missing() {
        let target = PathBuf::from(
            "/tmp/dotty_test_nonexistent_{}.txt".to_string() + &std::process::id().to_string(),
        );
        let repo_file = PathBuf::from("/tmp/dotty_repo_file.txt");
        assert!(inspect_target(&target, &repo_file) == TargetState::NeedsSymlink);
    }

    #[test]
    fn test_inspect_target_regular_file() {
        let dir = std::env::temp_dir().join(format!("dotty_inspect_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("file.txt");
        fs::write(&target, "content").unwrap();
        let repo_file = PathBuf::from("/tmp/repo.txt");

        assert!(inspect_target(&target, &repo_file) == TargetState::NeedsBackup);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_expand_target_ref_tilde() {
        let home = crate::convention::home_dir().unwrap();
        let path = expand_target_ref("~/.vimrc").unwrap();
        assert_eq!(path, home.join(".vimrc"));
    }

    #[test]
    fn test_expand_target_ref_absolute() {
        let path = expand_target_ref("/opt/nvim/appimage").unwrap();
        assert_eq!(path, PathBuf::from("/opt/nvim/appimage"));
    }

    #[test]
    fn test_rebuild_managed_map() {
        let files = vec!["base/home/.vimrc".into(), "base/home/.gitconfig".into()];
        let managed = rebuild_managed_map(&files);

        assert_eq!(managed.len(), 2);
        assert!(managed.contains_key("base/home/.vimrc"));
        assert!(managed.contains_key("base/home/.gitconfig"));
        assert!(managed.get("base/home/.vimrc").unwrap().starts_with("~"));
    }
}
