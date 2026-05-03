use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::convention::{
    KNOWN_PLATFORMS, read_config, resolve_repo_path, resolve_state_path, validate_machine_name,
    write_config,
};
use crate::git::{git_clone, git_init};
use crate::prompt::{prompt_input, prompt_select};

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

    let machine_name = if let Some(name) = machine {
        validate_machine_name(&name)?;
        name
    } else if let Some(url) = &git_url {
        // Clone mode: scan repo for known machines, prompt user
        clone_repo(url, &repo_path)?;
        prompt_machine_from_repo(&repo_path)?
    } else {
        // Fresh repo mode: prompt for machine name
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
fn create_fresh_repo(repo_path: &Path) -> Result<()> {
    // If repo already exists and is a git repo, inform user
    if repo_path.exists() && repo_path.join(".git").exists() {
        println!("Repo already exists at {}", repo_path.display());
        return Ok(());
    }

    fs::create_dir_all(repo_path)?;
    git_init(repo_path)?;

    // Create base/home/ directory
    let base_home = repo_path.join("base").join("home");
    fs::create_dir_all(&base_home)?;

    println!("Created fresh repo at {}", repo_path.display());
    Ok(())
}

/// Clone a repository into the resolved path.
fn clone_repo(url: &str, repo_path: &Path) -> Result<()> {
    // Pre-check: if directory exists and is not empty, abort
    if repo_path.exists() {
        let mut entries = fs::read_dir(repo_path)?;
        if entries.next().is_some() {
            anyhow::bail!(
                "Directory {} already exists and is not empty. \
                 Remove it or choose a different path via $DOTTY_HOME.",
                repo_path.display()
            );
        }
    }

    git_clone(url, repo_path)?;
    println!("Cloned repo into {}", repo_path.display());
    Ok(())
}

/// Ensure the state directory exists.
fn ensure_state_dir(state_path: &Path) -> Result<()> {
    fs::create_dir_all(state_path)?;
    Ok(())
}

/// Prompt the user for a machine name (fresh repo mode).
fn prompt_machine_name() -> Result<String> {
    let name = prompt_input("What is this machine called? (e.g. macbook, ubuntu-work)")?;
    validate_machine_name(&name)?;
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
fn prompt_machine_from_repo(repo_path: &Path) -> Result<String> {
    let known_machines = scan_machine_directories(repo_path);

    if known_machines.is_empty() {
        // No known machines in repo — just prompt for a name
        return prompt_machine_name();
    }

    // Build selection list: known machines + "(new)"
    let mut options: Vec<String> = known_machines;
    options.push("(new)".to_string());

    let selected = prompt_select(
        "Which machine is this?",
        &options.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    )?;

    if selected == options.len() - 1 {
        // User chose "(new)"
        prompt_machine_name()
    } else {
        Ok(options[selected].clone())
    }
}

/// Scan the repo for machine directories.
///
/// Returns a sorted list of directory names that look like machine tiers.
fn scan_machine_directories(repo_path: &Path) -> Vec<String> {
    let mut machines = Vec::new();

    let entries = match fs::read_dir(repo_path) {
        Ok(e) => e,
        Err(_) => return machines,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();

        // Must be a directory
        if !path.is_dir() {
            continue;
        }

        // Skip hidden directories (like .git)
        if name.starts_with('.') {
            continue;
        }

        // Skip base/
        if name == "base" {
            continue;
        }

        // Skip known platforms
        if KNOWN_PLATFORMS.iter().any(|&p| p == name) {
            continue;
        }

        // Must contain a home/ subdirectory
        let home_dir = path.join("home");
        if home_dir.is_dir() {
            machines.push(name);
        }
    }

    machines.sort();
    machines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("dotty_init_test_{}_{}", std::process::id(), id))
    }

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
}
