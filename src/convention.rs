use std::env;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::error::DottyError;

// Re-export from platform module
pub use crate::platform::{KNOWN_PLATFORMS, detect_platform};

// Re-export from config module
pub use crate::config::{Config, read_config, write_config};

/// Resolve the dotty repository path.
///
/// Checks `$DOTTY_HOME` first, falls back to `~/.dotty`.
pub fn resolve_repo_path() -> Result<PathBuf, DottyError> {
    if let Ok(path) = env::var("DOTTY_HOME") {
        return Ok(PathBuf::from(path));
    }
    Ok(home_dir()?.join(".dotty"))
}

/// Resolve the dotty state directory.
///
/// Checks `$DOTTY_STATE_HOME` first, then `$XDG_STATE_HOME/dotty`,
/// falls back to `~/.local/state/dotty`.
pub fn resolve_state_path() -> Result<PathBuf, DottyError> {
    if let Ok(path) = env::var("DOTTY_STATE_HOME") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(xdg) = env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(xdg).join("dotty"));
    }
    Ok(home_dir()?.join(".local").join("state").join("dotty"))
}

/// Convert a repo-relative path to its target (real filesystem) path.
///
/// E.g. `base/home/.vimrc` → `~/.vimrc`, `linux/opt/nvim/app` → `/opt/nvim/app`.
pub(crate) fn repo_to_target(repo_path: &Path) -> Result<PathBuf, DottyError> {
    let mut components = repo_path.components();

    // Skip the scope directory (base, macos, macbook, etc.)
    components.next();

    // The next component determines the target root
    let root_component = components.next().ok_or_else(|| {
        DottyError::Path(format!(
            "repo path has no root component: {}",
            repo_path.display()
        ))
    })?;

    let root = match root_component.as_os_str().to_str() {
        Some("home") => home_dir()?,
        Some(dir) => PathBuf::from("/").join(dir),
        None => {
            return Err(DottyError::Path(format!(
                "invalid root component in: {}",
                repo_path.display()
            )));
        }
    };

    let relative: PathBuf = components.as_path().to_path_buf();
    Ok(root.join(relative))
}

/// Convert a target (real filesystem) path to its repo-relative path.
///
/// E.g. `~/.vimrc` → `home/.vimrc`, `/opt/nvim/app` → `opt/nvim/app`.
///
/// Returns the path relative to the scope directory (without the scope prefix).
pub(crate) fn target_to_repo(target_path: &Path) -> Result<PathBuf, DottyError> {
    let home = home_dir()?;

    if let Ok(relative) = target_path.strip_prefix(&home) {
        return Ok(PathBuf::from("home").join(relative));
    }

    if let Ok(relative) = target_path.strip_prefix("/") {
        if relative.as_os_str().is_empty() {
            return Err(DottyError::Path(
                "cannot map root path \"/\" to repo".to_string(),
            ));
        }
        return Ok(relative.to_path_buf());
    }

    Err(DottyError::Path(format!(
        "cannot map target path: {}",
        target_path.display()
    )))
}

/// Return the user's home directory.
///
/// Uses `std::env::home_dir()` which consults platform-specific mechanisms
/// (not just `$HOME`), falling back to `/` only as a last resort.
pub fn home_dir() -> Result<PathBuf, DottyError> {
    std::env::home_dir().ok_or_else(|| DottyError::Config("cannot determine home directory".into()))
}

/// Generate a timestamp string for backup directories.
///
/// Format: `YYYY-MM-DDTHH-MM-SS-NNN` (e.g. `2024-01-15T10-30-00-847`).
/// The trailing 3-digit millisecond component prevents collisions when
/// two runs happen within the same second.
pub fn backup_timestamp() -> String {
    let now = chrono::Local::now();
    let millis = now.timestamp_subsec_millis();
    format!("{}-{:03}", now.format("%Y-%m-%dT%H-%M-%S"), millis)
}

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
        return Err(DottyError::Config("Machine name cannot be empty.".into()));
    }
    if name.starts_with('.') {
        return Err(DottyError::Config(
            "Machine name cannot start with a dot.".into(),
        ));
    }
    if name.contains("..") {
        return Err(DottyError::Config(
            "Machine name cannot contain '..'.".into(),
        ));
    }
    if name.contains('/') {
        return Err(DottyError::Config(
            "Machine name cannot contain '/'.".into(),
        ));
    }
    if name == "base" {
        return Err(DottyError::Config("'base' is a reserved name.".into()));
    }
    if KNOWN_PLATFORMS.contains(&name) {
        return Err(DottyError::Config(format!(
            "'{}' is a reserved platform name.",
            name
        )));
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

/// Recursively walk a directory and collect all file paths.
pub fn walk_dir(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), DottyError> {
    for dir_entry in fs::read_dir(dir)? {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();

        // is_file() follows symlinks; symlink_metadata checks the link itself
        let is_file_or_symlink = path.is_file()
            || path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink());

        if is_file_or_symlink {
            files.push(path);
        } else if path.is_dir() {
            walk_dir(&path, files)?;
        }
    }
    Ok(())
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
        let repo_path = PathBuf::from(file);
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

/// Expand `~` prefix in a path string to the full home directory path.
///
/// E.g. `"~/.vimrc"` → `/home/user/.vimrc`, `"/opt/app"` → `/opt/app`.
pub fn expand_tilde(path: &str) -> Result<PathBuf, DottyError> {
    let home = home_dir()?;

    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(home.join(rest));
    }
    if path == "~" {
        return Ok(home);
    }
    Ok(PathBuf::from(path))
}

/// Calculate the total size of a directory recursively in bytes.
pub fn calculate_dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Ok(meta) = fs::metadata(&path) {
                total += meta.len();
            }
        } else if path.is_dir() {
            total += calculate_dir_size(&path);
        }
    }

    total
}

/// List backup directory names sorted by name (chronological order).
pub fn list_backups(state_path: &Path) -> Vec<String> {
    let backup_dir = state_path.join("backups");

    if !backup_dir.is_dir() {
        return Vec::new();
    }

    let mut backups = Vec::new();

    for entry in fs::read_dir(&backup_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() {
            backups.push(name);
        }
    }

    backups.sort();
    backups
}

/// Parse a date string in YYYY-MM-DD format and return the corresponding
/// backup timestamp prefix for comparison.
///
/// Backup timestamps are in format YYYY-MM-DDTHH-MM-SS-NNN, so a date "2024-01-15"
/// matches all backups starting with "2024-01-15T".
pub fn date_to_backup_prefix(date: &str) -> Option<String> {
    if date.len() != 10 {
        return None;
    }
    // Basic validation: YYYY-MM-DD
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || parts[0].parse::<u32>().is_err()
        || parts[1].parse::<u32>().is_err()
        || parts[2].parse::<u32>().is_err()
    {
        return None;
    }
    Some(format!("{}T", date))
}

use std::fs;

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    // In Rust 2024, set_var/remove_var are unsafe (data-race risk in multithreaded code).
    // #[serial] ensures these tests run one at a time, preventing concurrent env mutations.
    fn set_env(key: &str, val: &str) {
        unsafe { env::set_var(key, val) };
    }
    fn remove_env(key: &str) {
        unsafe { env::remove_var(key) };
    }

    #[test]
    #[serial]
    fn test_resolve_repo_path_default() {
        remove_env("DOTTY_HOME");
        let path = resolve_repo_path().unwrap();
        assert!(path.ends_with(".dotty"));
    }

    #[test]
    #[serial]
    fn test_resolve_repo_path_custom() {
        set_env("DOTTY_HOME", "/custom/dotty/path");
        let path = resolve_repo_path().unwrap();
        assert_eq!(path, PathBuf::from("/custom/dotty/path"));
        remove_env("DOTTY_HOME");
    }

    #[test]
    #[serial]
    fn test_resolve_state_path_default() {
        remove_env("DOTTY_STATE_HOME");
        remove_env("XDG_STATE_HOME");
        let path = resolve_state_path().unwrap();
        assert!(path.ends_with(".local/state/dotty"));
    }

    #[test]
    #[serial]
    fn test_resolve_state_path_custom() {
        set_env("DOTTY_STATE_HOME", "/custom/state");
        let path = resolve_state_path().unwrap();
        assert_eq!(path, PathBuf::from("/custom/state"));
        remove_env("DOTTY_STATE_HOME");
    }

    #[test]
    #[serial]
    fn test_resolve_state_path_xdg() {
        remove_env("DOTTY_STATE_HOME");
        set_env("XDG_STATE_HOME", "/var/lib/state");
        let path = resolve_state_path().unwrap();
        assert_eq!(path, PathBuf::from("/var/lib/state/dotty"));
        remove_env("XDG_STATE_HOME");
    }

    #[test]
    fn test_repo_to_target_home() {
        let repo = Path::new("base/home/.vimrc");
        let target = repo_to_target(repo).unwrap();
        assert!(target.to_string_lossy().ends_with(".vimrc"));
        assert!(target.starts_with(&home_dir().unwrap()));
    }

    #[test]
    fn test_repo_to_target_opt() {
        let repo = Path::new("linux/opt/nvim/appimage");
        let target = repo_to_target(repo).unwrap();
        assert_eq!(target, PathBuf::from("/opt/nvim/appimage"));
    }

    #[test]
    fn test_repo_to_target_library() {
        let repo = Path::new("macos/Library/Preferences/com.example.plist");
        let target = repo_to_target(repo).unwrap();
        assert_eq!(
            target,
            PathBuf::from("/Library/Preferences/com.example.plist")
        );
    }

    #[test]
    fn test_repo_to_target_nested_home() {
        let repo = Path::new("base/home/.config/nvim/init.lua");
        let target = repo_to_target(repo).unwrap();
        assert!(target.to_string_lossy().ends_with(".config/nvim/init.lua"));
    }

    #[test]
    fn test_target_to_repo_home() {
        let home = home_dir().unwrap();
        let target = home.join(".vimrc");
        let repo = target_to_repo(&target).unwrap();
        assert_eq!(repo, PathBuf::from("home/.vimrc"));
    }

    #[test]
    fn test_target_to_repo_absolute() {
        let target = PathBuf::from("/opt/nvim/appimage");
        let repo = target_to_repo(&target).unwrap();
        assert_eq!(repo, PathBuf::from("opt/nvim/appimage"));
    }

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

    #[test]
    fn test_repo_to_target_invalid() {
        let repo = Path::new("base");
        assert!(repo_to_target(repo).is_err());
    }

    #[test]
    fn test_target_to_repo_root_path_returns_error() {
        let target = PathBuf::from("/");
        assert!(target_to_repo(&target).is_err());
    }

    #[test]
    fn test_backup_timestamp_format() {
        let ts = backup_timestamp();
        assert_eq!(ts.len(), 23, "timestamp length should be 23 (with millis)");
        assert!(ts.chars().nth(4) == Some('-'), "missing dash at position 4");
        assert!(ts.chars().nth(10) == Some('T'), "missing T at position 10");
        // Last 3 chars should be digits (milliseconds), preceded by '-'
        let millis = ts.rsplit('-').next().unwrap();
        assert_eq!(millis.len(), 3, "millis should be 3 digits");
        assert!(
            millis.chars().all(|c| c.is_ascii_digit()),
            "millis should be digits"
        );
    }
}
