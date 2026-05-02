use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::DottyError;

/// Known platform identifiers.
pub const KNOWN_PLATFORMS: &[&str] = &["macos", "linux", "freebsd"];

/// Resolve the dotty repository path.
///
/// Checks `$DOTTY_HOME` first, falls back to `~/.dotty`.
pub fn resolve_repo_path() -> Result<PathBuf, DottyError> {
    if let Ok(path) = env::var("DOTTY_HOME") {
        return Ok(PathBuf::from(path));
    }
    Ok(home_dir().join(".dotty"))
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
    Ok(home_dir().join(".local").join("state").join("dotty"))
}

#[allow(dead_code)]
/// Detect the current platform via `uname -s`.
///
/// Returns `Some("macos")`, `Some("linux")`, `Some("freebsd")`, or `None`
/// for unknown platforms.
pub fn detect_platform() -> Option<String> {
    let output = Command::new("uname").arg("-s").output().ok()?;
    let sysname = String::from_utf8(output.stdout).ok()?.trim().to_string();

    match sysname.as_str() {
        "Darwin" => Some("macos".into()),
        "Linux" => Some("linux".into()),
        "FreeBSD" => Some("freebsd".into()),
        _ => None,
    }
}

#[allow(dead_code)]
/// Convert a repo-relative path to its target (real filesystem) path.
///
/// E.g. `base/home/.vimrc` → `~/.vimrc`, `linux/opt/nvim/app` → `/opt/nvim/app`.
pub fn repo_to_target(repo_path: &Path) -> Result<PathBuf, DottyError> {
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
        Some("home") => home_dir(),
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

#[allow(dead_code)]
/// Convert a target (real filesystem) path to its repo-relative path.
///
/// E.g. `~/.vimrc` → `home/.vimrc`, `/opt/nvim/app` → `opt/nvim/app`.
///
/// Returns the path relative to the scope directory (without the scope prefix).
pub fn target_to_repo(target_path: &Path) -> Result<PathBuf, DottyError> {
    let home = home_dir();

    if let Ok(relative) = target_path.strip_prefix(&home) {
        return Ok(PathBuf::from("home").join(relative));
    }

    if let Ok(relative) = target_path.strip_prefix("/") {
        return Ok(
            PathBuf::from(relative.parent().unwrap_or_else(|| Path::new("")))
                .join(relative.file_name().unwrap()),
        );
    }

    Err(DottyError::Path(format!(
        "cannot map target path: {}",
        target_path.display()
    )))
}

/// Configuration stored in `config.toml` inside the state directory.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub machine: Option<String>,
    pub managed: HashMap<String, String>,
}

impl Config {
    /// Create a new empty config.
    pub fn new() -> Self {
        Self {
            machine: None,
            managed: HashMap::new(),
        }
    }

    /// Set the machine name.
    pub fn set_machine(&mut self, name: String) {
        self.machine = Some(name);
    }
}

/// Read `config.toml` from the state directory.
///
/// Returns a default (empty) config if the file doesn't exist.
pub fn read_config(state_path: &Path) -> Result<Config, DottyError> {
    let config_path = state_path.join("config.toml");
    if !config_path.exists() {
        return Ok(Config::new());
    }

    let content = std::fs::read_to_string(&config_path)?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}

/// Write `config.toml` to the state directory.
///
/// Creates the state directory if it doesn't exist.
pub fn write_config(state_path: &Path, config: &Config) -> Result<(), DottyError> {
    std::fs::create_dir_all(state_path)?;
    let content = toml::to_string_pretty(config)?;
    std::fs::write(state_path.join("config.toml"), content)?;
    Ok(())
}

/// Return the user's home directory.
fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // In Rust 2024, set_var/remove_var are unsafe (data-race risk in multithreaded code).
    // Tests are single-threaded, so this is safe.
    fn set_env(key: &str, val: &str) {
        unsafe { env::set_var(key, val) };
    }
    fn remove_env(key: &str) {
        unsafe { env::remove_var(key) };
    }

    #[test]
    fn test_resolve_repo_path_default() {
        remove_env("DOTTY_HOME");
        let path = resolve_repo_path().unwrap();
        assert!(path.ends_with(".dotty"));
    }

    #[test]
    fn test_resolve_repo_path_custom() {
        set_env("DOTTY_HOME", "/custom/dotty/path");
        let path = resolve_repo_path().unwrap();
        assert_eq!(path, PathBuf::from("/custom/dotty/path"));
        remove_env("DOTTY_HOME");
    }

    #[test]
    fn test_resolve_state_path_default() {
        remove_env("DOTTY_STATE_HOME");
        remove_env("XDG_STATE_HOME");
        let path = resolve_state_path().unwrap();
        assert!(path.ends_with(".local/state/dotty"));
    }

    #[test]
    fn test_resolve_state_path_custom() {
        set_env("DOTTY_STATE_HOME", "/custom/state");
        let path = resolve_state_path().unwrap();
        assert_eq!(path, PathBuf::from("/custom/state"));
        remove_env("DOTTY_STATE_HOME");
    }

    #[test]
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
        assert!(target.starts_with(&home_dir()));
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
        let home = home_dir();
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
}
