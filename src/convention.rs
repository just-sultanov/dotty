use std::path::Path;

use tracing::warn;

use crate::err_msg;
use crate::error::DottyError;
use crate::paths::repo_to_target;
use crate::platform::KNOWN_PLATFORMS;

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

/// A validated machine name.
///
/// Guarantees that the inner `String` satisfies all machine-name constraints.
/// Validation rules:
/// - Must not be empty or whitespace-only
/// - Must not start with a dot (hidden names)
/// - Must not contain `..` (path traversal)
/// - Must not contain `/` or `\\` (path separators)
/// - Must not be `base` (reserved)
/// - Must not match a known platform name
/// - Must only contain alphanumeric characters, hyphens, and underscores
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineName(String);

impl MachineName {
    /// Validate and construct a `MachineName`.
    pub fn new(name: &str) -> Result<Self, DottyError> {
        // Block empty or whitespace-only names
        if name.trim().is_empty() {
            return Err(DottyError::InvalidMachineName {
                name: name.to_string(),
                reason: err_msg!("machine name cannot be empty"),
            });
        }
        // Block hidden names (starting with dot)
        if name.starts_with('.') {
            return Err(DottyError::InvalidMachineName {
                name: name.to_string(),
                reason: err_msg!("machine name cannot start with a dot"),
            });
        }
        // Block parent directory references (path traversal prevention)
        if name.contains("..") {
            return Err(DottyError::InvalidMachineName {
                name: name.to_string(),
                reason: err_msg!("machine name cannot contain '..'"),
            });
        }
        // Block Unix path separators (prevents directory traversal)
        if name.contains('/') {
            return Err(DottyError::InvalidMachineName {
                name: name.to_string(),
                reason: err_msg!("machine name cannot contain '/'"),
            });
        }
        // Block Windows path separators (prevents cross-platform traversal)
        if name.contains('\\') {
            return Err(DottyError::InvalidMachineName {
                name: name.to_string(),
                reason: err_msg!("machine name cannot contain '\\'"),
            });
        }
        // Block reserved name 'base'
        if name == "base" {
            return Err(DottyError::InvalidMachineName {
                name: name.to_string(),
                reason: err_msg!("'base' is a reserved name"),
            });
        }
        // Block reserved platform names
        if KNOWN_PLATFORMS.contains(&name) {
            return Err(DottyError::InvalidMachineName {
                name: name.to_string(),
                reason: err_msg!("'{}' is a reserved platform name", name),
            });
        }
        // Whitelist validation: only allow alphanumeric, hyphens, and underscores
        // This blocks URL-encoded chars, control chars, null bytes, and any other invalid chars
        for c in name.chars() {
            if !c.is_alphanumeric() && c != '-' && c != '_' {
                return Err(DottyError::InvalidMachineName {
                    name: name.to_string(),
                    reason: err_msg!(
                        "machine name can only contain alphanumeric characters, hyphens, and underscores"
                    ),
                });
            }
        }
        Ok(MachineName(name.to_string()))
    }

    /// Consume the newtype and return the inner `String`.
    pub fn into_string(self) -> String {
        self.0
    }
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
    use crate::config::{Config, read_config, write_config};
    use std::fs;

    /// Create a unique temporary directory that is automatically cleaned up on drop.
    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // -- scan_machine_directories tests --

    #[test]
    fn test_scan_machine_directories_finds_machines() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
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
    }

    #[test]
    fn test_scan_machine_directories_empty_repo() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        fs::create_dir_all(base.join("base")).unwrap();

        let machines = scan_machine_directories(&base);
        assert!(machines.is_empty());
    }

    #[test]
    fn test_scan_machine_directories_sorted() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        fs::create_dir_all(base.join("zebra/home")).unwrap();
        fs::create_dir_all(base.join("alpha/home")).unwrap();
        fs::create_dir_all(base.join("middle/home")).unwrap();

        let machines = scan_machine_directories(&base);
        assert_eq!(machines, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn test_scan_skips_base_and_platforms() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        fs::create_dir_all(base.join("base/home")).unwrap();
        fs::create_dir_all(base.join("macos/home")).unwrap();
        fs::create_dir_all(base.join("linux/home")).unwrap();
        fs::create_dir_all(base.join("freebsd/home")).unwrap();
        fs::create_dir_all(base.join("my-machine/home")).unwrap();

        let machines = scan_machine_directories(&base);
        assert_eq!(machines, vec!["my-machine"]);
    }

    // -- validate_machine_name tests --

    #[test]
    fn test_validate_machine_name_rejects_empty() {
        assert!(MachineName::new("").is_err());
        assert!(MachineName::new("   ").is_err());
    }

    #[test]
    fn test_validate_machine_name_accepts_valid() {
        assert!(MachineName::new("macbook").is_ok());
        assert!(MachineName::new("ubuntu-work").is_ok());
    }

    #[test]
    fn test_validate_machine_name_rejects_slash() {
        assert!(MachineName::new("foo/bar").is_err());
        assert!(MachineName::new("foo/../bar").is_err());
    }

    #[test]
    fn test_validate_machine_name_rejects_backslash() {
        // Windows path separator should be rejected
        assert!(MachineName::new("foo\\bar").is_err());
        assert!(MachineName::new("..\\etc\\passwd").is_err());
    }

    #[test]
    fn test_validate_machine_name_rejects_dotdot_in_middle() {
        // Machine names with '..' in the middle should be rejected
        assert!(MachineName::new("mac..book").is_err());
        assert!(MachineName::new("test..machine").is_err());
    }

    #[test]
    fn test_validate_machine_name_rejects_url_encoded() {
        // URL-encoded '..' should be rejected
        assert!(MachineName::new("my%2e%2emachine").is_err());
        // URL-encoded '/..' should be rejected
        assert!(MachineName::new("test%2f..%2fetc").is_err());
    }

    #[test]
    fn test_validate_machine_name_rejects_null_byte() {
        // Null bytes should be rejected
        assert!(MachineName::new("machine\0hack").is_err());
    }

    #[test]
    fn test_validate_machine_name_rejects_control_chars() {
        // Control characters should be rejected
        assert!(MachineName::new("machine\tname").is_err());
        assert!(MachineName::new("machine\nname").is_err());
    }

    #[test]
    fn test_validate_machine_name_accepts_hyphens_and_underscores() {
        // Hyphens and underscores should be allowed
        assert!(MachineName::new("valid-machine-name").is_ok());
        assert!(MachineName::new("valid_machine_name").is_ok());
        assert!(MachineName::new("valid-machine_name123").is_ok());
    }

    // -- MachineName::new tests --

    #[test]
    fn test_machine_name_new_valid() {
        let m = MachineName::new("macbook").unwrap();
        assert_eq!(m.into_string(), "macbook");
    }

    #[test]
    fn test_machine_name_new_rejects_empty() {
        assert!(MachineName::new("").is_err());
        assert!(MachineName::new("   ").is_err());
    }

    #[test]
    fn test_machine_name_new_rejects_invalid_chars() {
        assert!(MachineName::new("foo/bar").is_err());
        assert!(MachineName::new("foo\\bar").is_err());
        assert!(MachineName::new("..").is_err());
        assert!(MachineName::new("machine\0hack").is_err());
        assert!(MachineName::new("machine\tname").is_err());
    }

    #[test]
    fn test_machine_name_new_rejects_reserved() {
        assert!(MachineName::new("base").is_err());
        assert!(MachineName::new("macos").is_err());
        assert!(MachineName::new("linux").is_err());
        assert!(MachineName::new(".hidden").is_err());
    }

    #[test]
    fn test_machine_name_new_equality() {
        let a = MachineName::new("my-machine").unwrap();
        let b = MachineName::new("my-machine").unwrap();
        let c = MachineName::new("other").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
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
        let dir = test_dir();
        let tmp = dir.path().to_path_buf();

        let mut config = Config::new();
        config.set_machine("macbook".into());
        config
            .managed
            .insert("base/home/.vimrc".into(), "~/.vimrc".into());

        write_config(&tmp, &config).unwrap();
        let read = read_config(&tmp).unwrap();

        assert_eq!(read.machine, Some("macbook".into()));
        assert!(read.managed.contains_key("base/home/.vimrc"));
    }

    #[test]
    fn test_read_config_missing_returns_default() {
        let dir = test_dir();
        let tmp = dir.path().to_path_buf();

        let config = read_config(&tmp).unwrap();
        assert!(config.machine.is_none());
        assert!(config.managed.is_empty());
    }
}
