use std::path::Path;

use tracing::warn;

use crate::error::DottyError;

// Re-export from platform module
pub use crate::platform::{KNOWN_PLATFORMS, detect_platform};

// Re-export from config module
pub use crate::config::{Config, read_config, write_config};

// Re-export from paths module
pub use crate::paths::{
    expand_tilde, home_dir, normalize_path, repo_to_target, resolve_repo_path, resolve_state_path,
    target_to_repo,
};

// Re-export from backups module
pub use crate::backups::{backup_timestamp, date_to_backup_prefix, list_backups};

// Re-export from fs_utils module
pub use crate::fs_utils::{calculate_dir_size, walk_dir};

/// Scan the repo for machine directories.
///
/// Returns a sorted list of directory names that look like machine tiers
/// (top-level dirs containing `home/`, excluding `base/` and known platforms).
pub fn scan_machine_directories(repo_path: &Path) -> Vec<String> {
    let mut machines = Vec::new();

    let entries = match std::fs::read_dir(repo_path) {
        Ok(e) => e,
        Err(e) => {
            warn!("cannot read repo directory {}: {}", repo_path.display(), e);
            return machines;
        }
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        if name == "base" {
            continue;
        }
        if KNOWN_PLATFORMS.iter().any(|&p| p == name) {
            continue;
        }
        if path.join("home").is_dir() {
            machines.push(name);
        }
    }

    machines.sort();
    machines
}

/// Validate a machine name.
///
/// Rejects empty names, reserved names (`base`, platform dirs), hidden names
/// (starting with `.`), and names containing path traversal (`..`).
pub fn validate_machine_name(name: &str) -> Result<(), DottyError> {
    if name.trim().is_empty() {
        return Err(DottyError::InvalidMachineName {
            name: name.to_string(),
            reason: "machine name cannot be empty".into(),
        });
    }
    if name.starts_with('.') {
        return Err(DottyError::InvalidMachineName {
            name: name.to_string(),
            reason: "machine name cannot start with a dot".into(),
        });
    }
    if name.contains("..") {
        return Err(DottyError::InvalidMachineName {
            name: name.to_string(),
            reason: "machine name cannot contain '..'".into(),
        });
    }
    if name.contains('/') {
        return Err(DottyError::InvalidMachineName {
            name: name.to_string(),
            reason: "machine name cannot contain '/'".into(),
        });
    }
    if name == "base" {
        return Err(DottyError::InvalidMachineName {
            name: name.to_string(),
            reason: "'base' is a reserved name".into(),
        });
    }
    if KNOWN_PLATFORMS.contains(&name) {
        return Err(DottyError::InvalidMachineName {
            name: name.to_string(),
            reason: format!("'{}' is a reserved platform name", name),
        });
    }
    Ok(())
}

/// Classify a repo-relative path into its tier.
///
/// Returns `Some("base")`, `Some("macos")`, `Some("macbook")`, etc.
pub fn classify_tier(
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
pub fn tier_priority(tier: &str) -> u32 {
    if tier == "base" {
        return 1;
    }
    if KNOWN_PLATFORMS.contains(&tier) {
        return 2;
    }
    3 // machine tier
}

/// Find all tracked repo files that manage the given target path.
///
/// If `machine_filter` is `Some`, only search within that machine tier.
/// Otherwise, search across all tiers.
pub fn find_managed_repo_files(
    target_path: &Path,
    tracked_files: &[String],
    machine_filter: Option<&str>,
) -> Vec<String> {
    let mut result = Vec::new();

    for file in tracked_files {
        let repo_path = std::path::PathBuf::from(file);
        if let Ok(target) = repo_to_target(&repo_path)
            && target == target_path
        {
            if let Some(filter) = machine_filter {
                let prefix = format!("{}/", filter);
                if file.starts_with(&prefix) {
                    result.push(file.clone());
                }
            } else {
                result.push(file.clone());
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "dotty_convention_test_{}_{}",
            std::process::id(),
            id
        ))
    }

    // -- scan_machine_directories tests --

    #[test]
    fn test_scan_machine_directories_finds_machines() {
        let base = unique_temp_dir();
        fs::create_dir_all(base.join("base/home")).unwrap();
        fs::create_dir_all(base.join("macos/home")).unwrap();
        fs::create_dir_all(base.join("linux/home")).unwrap();
        fs::create_dir_all(base.join("macbook/home")).unwrap();
        fs::create_dir_all(base.join("ubuntu-work/home")).unwrap();
        // This should NOT be detected as a machine (no home/)
        fs::create_dir_all(base.join("some-other-dir")).unwrap();
        // Hidden dir should be skipped
        fs::create_dir_all(base.join(".git")).unwrap();

        let machines = scan_machine_directories(&base);
        assert_eq!(machines, vec!["macbook", "ubuntu-work"]);

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn test_scan_machine_directories_empty_repo() {
        let base = unique_temp_dir();
        fs::create_dir_all(base.join("base")).unwrap();

        let machines = scan_machine_directories(&base);
        assert!(machines.is_empty());

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn test_scan_machine_directories_sorted() {
        let base = unique_temp_dir();
        fs::create_dir_all(base.join("zebra/home")).unwrap();
        fs::create_dir_all(base.join("alpha/home")).unwrap();
        fs::create_dir_all(base.join("middle/home")).unwrap();

        let machines = scan_machine_directories(&base);
        assert_eq!(machines, vec!["alpha", "middle", "zebra"]);

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn test_scan_skips_base_and_platforms() {
        let base = unique_temp_dir();
        fs::create_dir_all(base.join("base/home")).unwrap();
        fs::create_dir_all(base.join("macos/home")).unwrap();
        fs::create_dir_all(base.join("linux/home")).unwrap();
        fs::create_dir_all(base.join("freebsd/home")).unwrap();
        fs::create_dir_all(base.join("my-machine/home")).unwrap();

        let machines = scan_machine_directories(&base);
        assert_eq!(machines, vec!["my-machine"]);

        fs::remove_dir_all(&base).unwrap();
    }

    // -- validate_machine_name tests --

    #[test]
    fn test_validate_machine_name_rejects_empty() {
        assert!(validate_machine_name("").is_err());
        assert!(validate_machine_name("   ").is_err());
    }

    #[test]
    fn test_validate_machine_name_accepts_valid() {
        assert!(validate_machine_name("macbook").is_ok());
        assert!(validate_machine_name("ubuntu-work").is_ok());
    }

    #[test]
    fn test_validate_machine_name_rejects_slash() {
        assert!(validate_machine_name("foo/bar").is_err());
        assert!(validate_machine_name("foo/../bar").is_err());
    }

    // -- classify_tier tests --

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
    fn test_classify_tier_unknown() {
        assert_eq!(
            classify_tier(
                "random/file.txt",
                &Some("macbook".into()),
                &Some("macos".into())
            ),
            None
        );
    }

    // -- tier_priority tests --

    #[test]
    fn test_tier_priority() {
        assert_eq!(tier_priority("base"), 1);
        assert_eq!(tier_priority("macos"), 2);
        assert_eq!(tier_priority("linux"), 2);
        assert_eq!(tier_priority("freebsd"), 2);
        assert_eq!(tier_priority("macbook"), 3);
        assert_eq!(tier_priority("ubuntu-work"), 3);
    }

    // -- config roundtrip test (uses re-exported config functions) --

    #[test]
    fn test_config_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("dotty_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let mut config = Config::new();
        config.set_machine("macbook".into());
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        write_config(&tmp, &config).unwrap();
        let read = read_config(&tmp).unwrap();

        assert_eq!(read.machine, Some("macbook".into()));
        assert!(read.managed.contains_key("base/home/.vimrc"));

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_read_config_missing_returns_default() {
        let tmp = std::env::temp_dir().join(format!("dotty_test_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let config = read_config(&tmp).unwrap();
        assert!(config.machine.is_none());
        assert!(config.managed.is_empty());

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
