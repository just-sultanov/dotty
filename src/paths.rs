use std::env;
use std::path::{Path, PathBuf};

use crate::error::DottyError;
use dir_spec::state_home;

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
/// Checks `$DOTTY_STATE_HOME` first, then uses the platform-specific
/// state directory via `dir_spec::state_home()` (which respects
/// `$XDG_STATE_HOME` on Linux), falling back to the platform default.
///
/// Platform defaults:
/// - Linux: `~/.local/state/dotty` (or `$XDG_STATE_HOME/dotty`)
/// - macOS: `~/Library/Application Support/dotty`
/// - Windows: `%LOCALAPPDATA%/dotty`
pub fn resolve_state_path() -> Result<PathBuf, DottyError> {
    if let Ok(path) = env::var("DOTTY_STATE_HOME") {
        return Ok(PathBuf::from(path));
    }
    // dir_spec::state_home() checks XDG_STATE_HOME and falls back to
    // platform-specific defaults (Linux: ~/.local/state, macOS: ~/Library/Application Support)
    if let Some(state) = state_home() {
        return Ok(state.join("dotty"));
    }
    // Fallback if home dir can't be determined
    Ok(home_dir()?.join(".local").join("state").join("dotty"))
}

/// Convert a repo-relative path to its target (real filesystem) path.
///
/// E.g. `base/home/.vimrc` → `~/.vimrc`, `linux/opt/nvim/app` → `/opt/nvim/app`.
pub fn repo_to_target(repo_path: &Path) -> Result<PathBuf, DottyError> {
    let mut components = repo_path.components();

    // Skip the scope directory (base, macos, macbook, etc.)
    components.next();

    // The next component determines the target root
    let root_component = components
        .next()
        .ok_or_else(|| DottyError::InvalidRepoPath {
            path: repo_path.display().to_string(),
            reason: "repo path has no root component".into(),
        })?;

    let root = match root_component.as_os_str().to_str() {
        Some("home") => home_dir()?,
        Some(dir) => PathBuf::from("/").join(dir),
        None => {
            return Err(DottyError::InvalidRepoPath {
                path: repo_path.display().to_string(),
                reason: "root component is not valid UTF-8".into(),
            });
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
pub fn target_to_repo(target_path: &Path) -> Result<PathBuf, DottyError> {
    let home = home_dir()?;

    if let Ok(relative) = target_path.strip_prefix(&home) {
        return Ok(PathBuf::from("home").join(relative));
    }

    if let Ok(relative) = target_path.strip_prefix("/") {
        if relative.as_os_str().is_empty() {
            return Err(DottyError::InvalidTargetPath {
                path: "/".to_string(),
                reason: "cannot map root path to repo".into(),
            });
        }
        return Ok(relative.to_path_buf());
    }

    Err(DottyError::InvalidTargetPath {
        path: target_path.display().to_string(),
        reason: "path does not start with home directory or \"/\"".into(),
    })
}

/// Return the user's home directory.
///
/// Uses `std::env::home_dir()` which consults platform-specific mechanisms
/// (not just `$HOME`), falling back to `/` only as a last resort.
pub fn home_dir() -> Result<PathBuf, DottyError> {
    std::env::home_dir().ok_or_else(|| {
        DottyError::MissingHomeDirectory(
            "HOME environment variable not set and unable to determine user home directory".into(),
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_repo_path_default() {
        let path = temp_env::with_var_unset("DOTTY_HOME", || resolve_repo_path().unwrap());
        assert!(path.ends_with(".dotty"));
    }

    #[test]
    fn test_resolve_repo_path_custom() {
        let path = temp_env::with_var("DOTTY_HOME", Some("/custom/dotty/path"), || {
            resolve_repo_path().unwrap()
        });
        assert_eq!(path, PathBuf::from("/custom/dotty/path"));
    }

    #[test]
    fn test_resolve_state_path_default() {
        let path = temp_env::with_vars_unset(["DOTTY_STATE_HOME", "XDG_STATE_HOME"], || {
            resolve_state_path().unwrap()
        });
        // dir_spec uses platform-specific defaults:
        // Linux: ~/.local/state/dotty, macOS: ~/Library/Application Support/dotty
        #[cfg(target_os = "linux")]
        assert!(path.ends_with(".local/state/dotty"));
        #[cfg(target_os = "macos")]
        assert!(
            path.to_string_lossy()
                .contains("Library/Application Support/dotty")
        );
        // Always ends with /dotty
        assert!(path.ends_with("dotty"));
    }

    #[test]
    fn test_resolve_state_path_custom() {
        let path = temp_env::with_var("DOTTY_STATE_HOME", Some("/custom/state"), || {
            resolve_state_path().unwrap()
        });
        assert_eq!(path, PathBuf::from("/custom/state"));
    }

    #[test]
    fn test_resolve_state_path_xdg() {
        let path = temp_env::with_vars(
            [
                ("DOTTY_STATE_HOME", None::<&str>),
                ("XDG_STATE_HOME", Some("/var/lib/state")),
            ],
            || resolve_state_path().unwrap(),
        );
        assert_eq!(path, PathBuf::from("/var/lib/state/dotty"));
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
    fn test_expand_tilde_home() {
        let path = expand_tilde("~/.vimrc").unwrap();
        assert!(path.to_string_lossy().ends_with(".vimrc"));
        assert!(path.starts_with(&home_dir().unwrap()));
    }

    #[test]
    fn test_expand_tilde_tilde_only() {
        let path = expand_tilde("~").unwrap();
        assert_eq!(path, home_dir().unwrap());
    }

    #[test]
    fn test_expand_tilde_absolute() {
        let path = expand_tilde("/opt/nvim/appimage").unwrap();
        assert_eq!(path, PathBuf::from("/opt/nvim/appimage"));
    }
}
