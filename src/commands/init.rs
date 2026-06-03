use std::fs;
use std::path::Path;

use crate::config::{read_config, write_config};
use crate::convention::{MachineName, scan_machine_directories};
use crate::error::DottyError;
use crate::git::{git_clone, git_init};
use crate::paths::{resolve_repo_path, resolve_state_path};
use crate::prompt::prompt_machine_selection;

/// Result type for init commands, using domain-specific `DottyError`.
type Result<T> = std::result::Result<T, DottyError>;

/// Bootstrap a new dotty repository or clone an existing one.
///
/// - Without `git_url`: creates a fresh repo (`git init`), sets up `base/home/`.
/// - With `git_url`: clones the repo, then sets up machine config.
///
/// In both cases, the machine name is either taken from the `machine` parameter
/// or prompted interactively.
pub fn run(git_url: Option<String>, machine: Option<String>) -> Result<()> {
    let repo_path = resolve_repo_path()?;
    let state_path = resolve_state_path()?;

    let machine_name = if let Some(url) = &git_url {
        // Clone mode: clone repo, then resolve machine name
        clone_repo(url, &repo_path)?;
        if let Some(name) = machine {
            MachineName::new(&name)?.into_string()
        } else {
            prompt_machine_from_repo(&repo_path)?
        }
    } else if let Some(name) = machine {
        // Fresh repo with explicit machine name (no prompt)
        create_fresh_repo(&repo_path)?;
        MachineName::new(&name)?.into_string()
    } else {
        // Fresh repo: prompt for machine name
        create_fresh_repo(&repo_path)?;
        prompt_machine_name()?
    };

    // Save machine name to config
    ensure_state_dir(&state_path)?;
    let mut config = read_config(&state_path)?;
    config.set_machine(machine_name.clone());
    write_config(&state_path, &config)?;

    println!("Machine set to: {}", machine_name);
    println!("Repo: {}", repo_path.display());
    Ok(())
}

/// Create a fresh repository: `git init` + `base/home/`.
///
/// Returns early (Ok) if `.git` already exists at the target path.
/// IO errors (create_dir_all) map to `DottyError::Io`.
/// Git errors (git_init) map to `DottyError::Git`.
fn create_fresh_repo(repo_path: &Path) -> Result<()> {
    // If repo already exists and is a git repo, inform user
    if repo_path.exists() && repo_path.join(".git").exists() {
        println!("Repo already exists at {}", repo_path.display());
        return Ok(());
    }

    fs::create_dir_all(repo_path).map_err(DottyError::Io)?;
    git_init(repo_path)?;

    // Create base/home/ directory
    let base_home = repo_path.join("base").join("home");
    fs::create_dir_all(&base_home).map_err(DottyError::Io)?;

    println!("Created fresh repo at {}", repo_path.display());
    Ok(())
}

/// Clone a repository into the resolved path.
///
/// Fails with `DottyError::GitAlreadyInitialized` if the target path
/// already contains a `.git` directory. Fails with
/// `DottyError::InitDirectoryNotEmpty` if the target directory exists
/// and contains files. IO errors from `read_dir` map to `DottyError::Io`.
/// Clone errors (git_clone) map to `DottyError::Git`.
fn clone_repo(url: &str, repo_path: &Path) -> Result<()> {
    // Pre-check: if the path already has a .git directory, abort with a
    // specific error so the user knows to use `dotty init` without a URL.
    if repo_path.join(".git").exists() {
        return Err(DottyError::GitAlreadyInitialized {
            path: repo_path.display().to_string(),
        });
    }

    // Pre-check: if directory exists and is not empty, abort
    if repo_path.exists() {
        let mut entries = fs::read_dir(repo_path).map_err(DottyError::Io)?;
        if entries.next().is_some() {
            return Err(DottyError::InitDirectoryNotEmpty {
                path: repo_path.display().to_string(),
            });
        }
    }

    git_clone(url, repo_path)?;
    println!("Cloned repo into {}", repo_path.display());
    Ok(())
}

/// Ensure the state directory exists.
/// IO errors map to `DottyError::Io`.
fn ensure_state_dir(state_path: &Path) -> Result<()> {
    fs::create_dir_all(state_path).map_err(DottyError::Io)?;
    Ok(())
}

/// Prompt the user for a machine name (fresh repo mode).
/// Prompt errors map to `DottyError::Prompt`; validation errors to `DottyError::InvalidMachineName`.
fn prompt_machine_name() -> Result<String> {
    let name =
        crate::prompt::prompt_input("What is this machine called? (e.g. macbook, ubuntu-work)")?;
    MachineName::new(&name)?;
    Ok(name)
}

/// Scan the repo for known machine directories and prompt the user to select one.
///
/// A "machine directory" is a top-level directory that:
/// - Contains a `home/` subdirectory
/// - Is not `base/`
/// - Is not a known platform (`macos/`, `linux/`, `freebsd/`)
///
/// If no known machines are found, falls back to prompting for a new name.
/// Validation errors map to `DottyError::InvalidMachineName`.
fn prompt_machine_from_repo(repo_path: &Path) -> Result<String> {
    let known_machines = scan_machine_directories(repo_path);
    let name = prompt_machine_selection(&known_machines)?;
    MachineName::new(&name)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp directory for test isolation.
    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Helper: initialize a minimal git repo at the given path.
    fn init_git_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        std::process::Command::new("git")
            .current_dir(path)
            .args(["init"])
            .output()
            .unwrap();
    }

    // -- create_fresh_repo tests --

    #[test]
    fn test_create_fresh_repo_creates_structure() {
        let dir = test_dir();
        let repo_path = dir.path().join("fresh");
        create_fresh_repo(&repo_path).unwrap();

        assert!(repo_path.join(".git").is_dir());
        assert!(repo_path.join("base/home").is_dir());
    }

    #[test]
    fn test_create_fresh_repo_idempotent_when_git_exists() {
        let dir = test_dir();
        let repo_path = dir.path().join("existing");
        create_fresh_repo(&repo_path).unwrap();
        assert!(repo_path.join(".git").is_dir());

        // Second call should succeed (idempotent)
        create_fresh_repo(&repo_path).unwrap();
        assert!(repo_path.join(".git").is_dir());
    }

    #[test]
    fn test_create_fresh_repo_existing_git_dir() {
        let dir = test_dir();
        let repo_path = dir.path().join("has_git");
        init_git_repo(&repo_path);

        // Already has .git, should return Ok without error
        create_fresh_repo(&repo_path).unwrap();
    }

    // -- clone_repo pre-check tests --

    #[test]
    fn test_clone_repo_fails_when_git_exists() {
        let dir = test_dir();
        let repo_path = dir.path().join("has_git");
        init_git_repo(&repo_path);

        let result = clone_repo("https://example.com/repo.git", &repo_path);
        assert!(result.is_err());
        match result.unwrap_err() {
            DottyError::GitAlreadyInitialized { path } => {
                assert!(path.contains("has_git"));
            }
            _ => panic!("expected GitAlreadyInitialized error"),
        }
    }

    #[test]
    fn test_clone_repo_fails_when_dir_not_empty() {
        let dir = test_dir();
        let repo_path = dir.path().join("nonempty");
        fs::create_dir_all(&repo_path).unwrap();
        fs::write(repo_path.join("stale_file"), "content").unwrap();

        let result = clone_repo("https://example.com/repo.git", &repo_path);
        assert!(result.is_err());
        match result.unwrap_err() {
            DottyError::InitDirectoryNotEmpty { path } => {
                assert!(path.contains("nonempty"));
            }
            _ => panic!("expected InitDirectoryNotEmpty error"),
        }
    }

    // -- ensure_state_dir tests --

    #[test]
    fn test_ensure_state_dir_creates_directory() {
        let dir = test_dir();
        let state_path = dir.path().join("state/deep/nested");
        assert!(!state_path.exists());

        ensure_state_dir(&state_path).unwrap();
        assert!(state_path.is_dir());
    }

    #[test]
    fn test_ensure_state_dir_succeeds_when_exists() {
        let dir = test_dir();
        let state_path = dir.path().join("existing");
        fs::create_dir_all(&state_path).unwrap();

        ensure_state_dir(&state_path).unwrap();
        assert!(state_path.is_dir());
    }
}
