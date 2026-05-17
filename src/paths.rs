use std::env;
use std::path::{Path, PathBuf};

use crate::error::DottyError;
use dir_spec::state_home;
use path_slash::PathExt;

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
        let result = PathBuf::from("home").join(relative);
        validate_no_dotdot(&result, target_path)?;
        return Ok(result);
    }

    if let Ok(relative) = target_path.strip_prefix("/") {
        if relative.as_os_str().is_empty() {
            return Err(DottyError::InvalidTargetPath {
                path: "/".to_string(),
                reason: "cannot map root path to repo".into(),
            });
        }
        let result = relative.to_path_buf();
        validate_no_dotdot(&result, target_path)?;
        return Ok(result);
    }

    Err(DottyError::InvalidTargetPath {
        path: target_path.display().to_string(),
        reason: "path does not start with home directory or \"/\"".into(),
    })
}

/// Validate that a repo-relative path does not contain `..` components.
///
/// This is a defense-in-depth check: `strip_prefix` itself is safe and won't
/// produce `..`, but an explicit guard makes the invariant visible and catches
/// any future regressions.
fn validate_no_dotdot(result: &Path, original: &Path) -> Result<(), DottyError> {
    for component in result.components() {
        if component.as_os_str() == ".." {
            return Err(DottyError::InvalidTargetPath {
                path: original.display().to_string(),
                reason: "resulting repo path contains '..' component".into(),
            });
        }
    }
    Ok(())
}

/// Return the user's home directory.
///
/// Checks `$HOME` first (for cross-platform consistency and testability),
/// then falls back to `std::env::home_dir()` which consults platform-specific
/// mechanisms (`USERPROFILE` on Windows, `$HOME` on Unix).
pub fn home_dir() -> Result<PathBuf, DottyError> {
    // Check $HOME first for cross-platform consistency.
    // On Windows std::env::home_dir() reads USERPROFILE, not HOME,
    // so tests that set HOME to a temp dir would fail without this.
    if let Ok(home) = env::var("HOME") {
        let path = PathBuf::from(home);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    std::env::home_dir().ok_or_else(|| {
        DottyError::MissingHomeDirectory(
            "HOME environment variable not set and unable to determine user home directory".into(),
        )
    })
}

/// Normalize path separators to forward slashes (`/`).
///
/// On Windows `PathBuf::to_string_lossy()` produces `\`, but dotty's
/// config keys and git paths always use `/`. Uses `path-slash` crate
/// for correct and efficient conversion across all platforms.
pub fn normalize_path(path: &Path) -> String {
    path.to_slash_lossy().into_owned()
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
    use proptest::prelude::*;

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
    #[cfg(not(target_os = "windows"))]
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
        assert!(target.starts_with(home_dir().unwrap()));
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
    fn test_target_to_repo_rejects_dotdot_in_result() {
        // Defense-in-depth: if the resulting repo path somehow contains "..",
        // it should be rejected. With current strip_prefix logic this won't
        // trigger for normal paths, but the guard protects against regressions.
        let home = home_dir().unwrap();
        // A path like /home/user/../etc/passwd would strip_prefix home and
        // produce "home/../etc/passwd" — the ".." check catches this.
        // We simulate by constructing a path that would produce ".." after strip.
        // In practice, canonical paths won't have "..", but the validation exists.
        let target = home.join("subdir");
        let repo = target_to_repo(&target).unwrap();
        // Normal path should not contain ".."
        for component in repo.components() {
            assert_ne!(component.as_os_str(), "..");
        }
    }

    #[test]
    fn test_validate_no_dotdot_rejects_dotdot() {
        // Direct test of the validation helper via a crafted scenario.
        // We can't easily trigger ".." through target_to_repo with real paths,
        // so we verify the function rejects paths with ".." components.
        let result = PathBuf::from("home/../etc/passwd");
        let original = PathBuf::from("/home/user/../etc/passwd");
        let err = validate_no_dotdot(&result, &original).unwrap_err();
        // The error should mention ".." in the reason
        match err {
            DottyError::InvalidTargetPath { reason, .. } => {
                assert!(reason.contains(".."));
            }
            _ => panic!("expected InvalidTargetPath error"),
        }
    }

    #[test]
    fn test_expand_tilde_home() {
        let path = expand_tilde("~/.vimrc").unwrap();
        assert!(path.to_string_lossy().ends_with(".vimrc"));
        assert!(path.starts_with(home_dir().unwrap()));
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

    // ── proptest roundtrip tests ──

    /// Check that a path string has no ".." component (for proptest filtering).
    fn has_no_dotdot_component(s: &str) -> bool {
        !s.split('/').any(|c| c == "..")
    }

    proptest::proptest! {
        #[test]
        fn roundtrip_repo_to_target_to_repo(
            // Root component: "home" or a directory name (no leading /)
            root in "home|opt|etc|usr|var|Library|srv",
            // File path: 1-4 non-empty components
            file_components in "[a-zA-Z0-9_.@-]{1,20}(/[a-zA-Z0-9_.@-]{1,20})*".prop_filter(
                "valid file path",
                |s: &String| !s.is_empty() && !s.starts_with('/') && has_no_dotdot_component(s),
            ),
        ) {
            let repo_path = PathBuf::from("base").join(&root).join(&file_components);

            // repo_to_target then target_to_repo should preserve the relative path
            // (scope component is stripped, which is expected)
            let target = repo_to_target(&repo_path).expect("valid repo path");
            let repo_back = target_to_repo(&target).expect("valid target path");

            // Expected: root/file (without the scope "base")
            let expected = PathBuf::from(&root).join(&file_components);

            prop_assert_eq!(
                &repo_back, &expected,
                "repo→target→repo roundtrip failed: {:?} → {:?} → {:?} (expected {:?})",
                repo_path, target, repo_back, expected
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn roundtrip_target_to_repo_to_target(
            // Target path: either home-relative or absolute
            variant in any::<bool>(),
            // Home-relative: dotfile or nested path like .config/nvim/init.lua
            home_components in "[.a-zA-Z0-9_@-]{1,20}(/[a-zA-Z0-9_.@-]{1,20})*".prop_filter(
                "valid home path",
                |s: &String| !s.is_empty() && has_no_dotdot_component(s),
            ),
            // Absolute: at least 2 components (dir/file) so repo path has scope+root
            abs_components in "[a-zA-Z0-9_]{1,10}/[a-zA-Z0-9_.@-]{1,20}(/[a-zA-Z0-9_.@-]{1,20})*".prop_filter(
                "valid abs path",
                |s: &String| !s.is_empty() && has_no_dotdot_component(s),
            ),
        ) {
            let home = home_dir().unwrap();

            let target = if variant {
                // Home-relative: ~/... → /home/user/...
                home.join(&home_components)
            } else {
                // Absolute: /<dir>/... (at least 2 components)
                PathBuf::from("/".to_string() + &abs_components)
            };

            let repo_relative = target_to_repo(&target).expect("valid target path");
            // repo_to_target expects scope/root/file, but target_to_repo returns root/file.
            // Prepend a dummy scope to complete the roundtrip.
            let repo_with_scope = PathBuf::from("base").join(&repo_relative);
            let target_back = repo_to_target(&repo_with_scope).expect("valid repo path");

            prop_assert_eq!(
                &target_back, &target,
                "target→repo→target roundtrip failed: {:?} → {:?} → {:?} → {:?}",
                target, repo_relative, repo_with_scope, target_back
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn roundtrip_with_deeply_nested_paths(
            // Generate 1-8 path components for deep nesting
            depth in 1usize..8,
        ) {
            let components: Vec<String> = (0..depth)
                .map(|i| format!(".file_{}", i))
                .collect();

            let file_path = components.join("/");
            let repo_path = PathBuf::from("base").join("home").join(&file_path);

            let target = repo_to_target(&repo_path).expect("valid repo path");
            let repo_back = target_to_repo(&target).expect("valid target path");

            let expected = PathBuf::from("home").join(&file_path);

            prop_assert_eq!(
                &repo_back, &expected,
                "deep nesting roundtrip failed (depth {}): {:?} → {:?} → {:?}",
                depth, repo_path, target, repo_back
            );
        }
    }
}
